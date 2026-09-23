use std::{
    convert::TryFrom,
    fmt,
    iter::{IntoIterator, Iterator},
    str::FromStr,
};

use http::{
    header::{self, HeaderName, HeaderValue},
    uri::{self, Authority, Parts, PathAndQuery, Scheme, Uri},
    Extensions, HeaderMap, Method, StatusCode,
};

use crate::{ext::Protocol, qpack::HeaderField};

#[derive(Debug)]
#[cfg_attr(test, derive(PartialEq, Clone))]
pub struct Header {
    pseudo: Pseudo,
    fields: HeaderMap,
}

#[allow(clippy::len_without_is_empty)]
impl Header {
    /// Creates a new `Header` frame data suitable for sending a request
    pub fn request(
        method: Method,
        uri: Uri,
        fields: HeaderMap,
        ext: Extensions,
    ) -> Result<Self, HeaderError> {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //# An endpoint MUST NOT generate
        //# an HTTP/3 field section containing connection-specific fields; any
        //# message containing connection-specific fields MUST be treated as
        //# malformed.
        for (name, val) in &fields {
            let name_bytes = name.as_str().as_bytes();
            if is_connection_specific(name_bytes) {
                return Err(HeaderError::ConnectionSpecificHeader(name.as_str().into()));
            }
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
            //# The only exception to this is the TE header field, which MAY be
            //# present in an HTTP/3 request header; when it is, it MUST NOT contain
            //# any value other than "trailers".
            if name_bytes == b"te" {
                let val_bytes = trim_ascii_whitespace(val.as_bytes());
                if !val_bytes.eq_ignore_ascii_case(b"trailers") {
                    return Err(HeaderError::InvalidTeHeader);
                }
            }
        }

        match (uri.authority(), fields.get("host")) {
            (None, None) => Err(HeaderError::MissingAuthority),
            (Some(a), Some(h)) if a.as_str() != h => Err(HeaderError::ContradictedAuthority),
            _ => Ok(Self {
                pseudo: Pseudo::request(method, uri, ext),
                fields,
            }),
        }
    }

    pub fn response(status: StatusCode, fields: HeaderMap) -> Result<Self, HeaderError> {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //# An endpoint MUST NOT generate
        //# an HTTP/3 field section containing connection-specific fields; any
        //# message containing connection-specific fields MUST be treated as
        //# malformed.
        for (name, _) in &fields {
            let name_bytes = name.as_str().as_bytes();
            if is_connection_specific(name_bytes) || name_bytes == b"te" {
                return Err(HeaderError::ConnectionSpecificHeader(name.as_str().into()));
            }
        }
        Ok(Self {
            pseudo: Pseudo::response(status),
            fields,
        })
    }

    pub fn trailer(fields: HeaderMap) -> Result<Self, HeaderError> {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //# An endpoint MUST NOT generate
        //# an HTTP/3 field section containing connection-specific fields; any
        //# message containing connection-specific fields MUST be treated as
        //# malformed.
        for (name, _) in &fields {
            let name_bytes = name.as_str().as_bytes();
            if is_connection_specific(name_bytes) || name_bytes == b"te" {
                return Err(HeaderError::ConnectionSpecificHeader(name.as_str().into()));
            }
        }
        Ok(Self {
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
            //# Pseudo-header fields MUST NOT appear in trailer
            //# sections.
            pseudo: Pseudo::default(),
            fields,
        })
    }

    pub fn into_request_parts(
        self,
    ) -> Result<(Method, Uri, Option<Protocol>, HeaderMap), HeaderError> {
        let mut uri = Uri::builder();

        if let Some(path) = self.pseudo.path {
            uri = uri.path_and_query(path.as_str().as_bytes());
        }

        if let Some(scheme) = self.pseudo.scheme {
            uri = uri.scheme(scheme.as_str().as_bytes());
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //# If the :scheme pseudo-header field identifies a scheme that has a
        //# mandatory authority component (including "http" and "https"), the
        //# request MUST contain either an :authority pseudo-header field or a
        //# Host header field.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=TODO
        //# If the scheme does not have a mandatory authority component and none
        //# is provided in the request target, the request MUST NOT contain the
        //# :authority pseudo-header or Host header fields.
        match (self.pseudo.authority, self.fields.get("host")) {
            (None, None) => return Err(HeaderError::MissingAuthority),
            (Some(a), None) => uri = uri.authority(a.as_str().as_bytes()),
            (None, Some(h)) => uri = uri.authority(h.as_bytes()),
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
            //# If both fields are present, they MUST contain the same value.
            (Some(a), Some(h)) if a.as_str() != h => {
                return Err(HeaderError::ContradictedAuthority)
            }
            (Some(_), Some(h)) => uri = uri.authority(h.as_bytes()),
        }

        Ok((
            self.pseudo.method.ok_or(HeaderError::MissingMethod)?,
            // When empty host field is built into an uri it fails
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
            //# If these fields are present, they MUST NOT be
            //# empty.
            uri.build().map_err(HeaderError::InvalidRequest)?,
            self.pseudo.protocol,
            self.fields,
        ))
    }

    pub fn into_response_parts(self) -> Result<(StatusCode, HeaderMap), HeaderError> {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //# The only exception to this is the TE header field, which MAY be
        //# present in an HTTP/3 request header; when it is, it MUST NOT contain
        //# any value other than "trailers".
        if self.fields.contains_key("te") {
            return Err(HeaderError::ConnectionSpecificHeader("te".into()));
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.2
        //= type=implication
        //# For responses, a single ":status" pseudo-header field is defined that
        //# carries the HTTP status code; see Section 15 of [HTTP].  This pseudo-
        //# header field MUST be included in all responses; otherwise, the
        //# response is malformed (see Section 4.1.2).
        Ok((
            self.pseudo.status.ok_or(HeaderError::MissingStatus)?,
            self.fields,
        ))
    }

    pub fn into_trailer_parts(self) -> Result<HeaderMap, HeaderError> {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
        //# Pseudo-header fields MUST NOT appear in trailer
        //# sections.
        if self.pseudo.len() > 0 {
            return Err(HeaderError::PseudoInTrailer);
        }
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //# The only exception to this is the TE header field, which MAY be
        //# present in an HTTP/3 request header; when it is, it MUST NOT contain
        //# any value other than "trailers".
        if self.fields.contains_key("te") {
            return Err(HeaderError::ConnectionSpecificHeader("te".into()));
        }
        Ok(self.fields)
    }

    pub fn into_fields(self) -> HeaderMap {
        self.fields
    }

    pub fn len(&self) -> usize {
        self.pseudo.len() + self.fields.len()
    }

    pub fn size(&self) -> usize {
        self.pseudo.len() + self.fields.len()
    }

    #[cfg(test)]
    pub(crate) fn authory_mut(&mut self) -> &mut Option<Authority> {
        &mut self.pseudo.authority
    }
}

impl IntoIterator for Header {
    type Item = HeaderField;
    type IntoIter = HeaderIter;
    fn into_iter(self) -> Self::IntoIter {
        HeaderIter {
            pseudo: Some(self.pseudo),
            last_header_name: None,
            fields: self.fields.into_iter(),
        }
    }
}

pub struct HeaderIter {
    pseudo: Option<Pseudo>,
    last_header_name: Option<HeaderName>,
    fields: header::IntoIter<HeaderValue>,
}

impl Iterator for HeaderIter {
    type Item = HeaderField;

    fn next(&mut self) -> Option<Self::Item> {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
        //# All pseudo-header fields MUST appear in the header section before
        //# regular header fields.
        if let Some(ref mut pseudo) = self.pseudo {
            if let Some(method) = pseudo.method.take() {
                return Some((":method", method.as_str()).into());
            }

            if let Some(scheme) = pseudo.scheme.take() {
                return Some((":scheme", scheme.as_str().as_bytes()).into());
            }

            if let Some(authority) = pseudo.authority.take() {
                return Some((":authority", authority.as_str().as_bytes()).into());
            }

            if let Some(path) = pseudo.path.take() {
                return Some((":path", path.as_str().as_bytes()).into());
            }

            if let Some(status) = pseudo.status.take() {
                return Some((":status", status.as_str()).into());
            }

            if let Some(protocol) = pseudo.protocol.take() {
                return Some((":protocol", protocol.as_str().as_bytes()).into());
            }
        }

        self.pseudo = None;

        for (new_header_name, header_value) in self.fields.by_ref() {
            if let Some(new) = new_header_name {
                self.last_header_name = Some(new);
            }
            if let (Some(ref n), v) = (&self.last_header_name, header_value) {
                return Some((n.as_str(), v.as_bytes()).into());
            }
        }

        None
    }
}

impl TryFrom<Vec<HeaderField>> for Header {
    type Error = HeaderError;
    fn try_from(headers: Vec<HeaderField>) -> Result<Self, Self::Error> {
        let mut fields: HeaderMap = HeaderMap::with_capacity(headers.len());
        let mut pseudo = Pseudo::default();
        let mut regular_field_seen = false;

        for field in headers.into_iter() {
            let (name, value) = field.into_inner();
            match Field::parse(name, value)? {
                //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
                //# Any request or response that contains a
                //# pseudo-header field that appears in a header section after a regular
                //# header field MUST be treated as malformed.
                Field::Method(_)
                | Field::Scheme(_)
                | Field::Authority(_)
                | Field::Path(_)
                | Field::Status(_)
                | Field::Protocol(_)
                    if regular_field_seen =>
                {
                    return Err(HeaderError::PseudoAfterRegularField)
                }
                Field::Method(m) => {
                    pseudo.method = Some(m);
                    pseudo.len += 1;
                }
                Field::Scheme(s) => {
                    pseudo.scheme = Some(s);
                    pseudo.len += 1;
                }
                Field::Authority(a) => {
                    pseudo.authority = Some(a);
                    pseudo.len += 1;
                }
                Field::Path(p) => {
                    pseudo.path = Some(p);
                    pseudo.len += 1;
                }
                Field::Status(s) => {
                    pseudo.status = Some(s);
                    pseudo.len += 1;
                }
                Field::Header((n, v)) => {
                    regular_field_seen = true;
                    //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.1
                    //# If a decompressed field
                    //# section contains multiple cookie field lines, these MUST be
                    //# concatenated into a single byte string using the two-byte delimiter
                    //# of "; " (ASCII 0x3b, 0x20) before being passed into a context other
                    //# than HTTP/2 or HTTP/3, such as an HTTP/1.1 connection, or a generic
                    //# HTTP server application.
                    if n == header::COOKIE {
                        match fields.entry(header::COOKIE) {
                            header::Entry::Occupied(mut entry) => {
                                let mut joined = Vec::with_capacity(
                                    entry.get().as_bytes().len() + 2 + v.as_bytes().len(),
                                );
                                joined.extend_from_slice(entry.get().as_bytes());
                                joined.extend_from_slice(b"; ");
                                joined.extend_from_slice(v.as_bytes());
                                let new_value = HeaderValue::from_bytes(&joined)
                                    .map_err(|_| HeaderError::invalid_value(&n, v.as_bytes()))?;
                                entry.insert(new_value);
                            }
                            header::Entry::Vacant(entry) => {
                                entry.insert(v);
                            }
                        }
                    } else {
                        fields.append(n, v);
                    }
                }
                Field::Protocol(p) => {
                    pseudo.protocol = Some(p);
                    pseudo.len += 1;
                }
            }
        }

        Ok(Header { pseudo, fields })
    }
}

enum Field {
    Method(Method),
    Scheme(Scheme),
    Authority(Authority),
    Path(PathAndQuery),
    Status(StatusCode),
    Protocol(Protocol),
    Header((HeaderName, HeaderValue)),
}

impl Field {
    fn parse<N, V>(name: N, value: V) -> Result<Self, HeaderError>
    where
        N: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        let name = name.as_ref();
        if name.is_empty() {
            return Err(HeaderError::InvalidHeaderName("name is empty".into()));
        }

        //= https://www.rfc-editor.org/rfc/rfc9114#section-10.3
        //# Requests or responses containing invalid field names MUST be treated
        //# as malformed.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-10.3
        //# Any request or response that contains a
        //# character not permitted in a field value MUST be treated as
        //# malformed.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //= type=implication
        //# A request or
        //# response containing uppercase characters in field names MUST be
        //# treated as malformed.

        if name[0] != b':' {
            let header_name = HeaderName::from_lowercase(name)
                .map_err(|_| HeaderError::invalid_name(name))?;

            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
            //# An endpoint MUST NOT generate
            //# an HTTP/3 field section containing connection-specific fields; any
            //# message containing connection-specific fields MUST be treated as
            //# malformed.
            let name_bytes = header_name.as_str().as_bytes();
            if is_connection_specific(name_bytes) {
                return Err(HeaderError::ConnectionSpecificHeader(
                    header_name.as_str().to_owned(),
                ));
            }

            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
            //# The only exception to this is the TE header field, which MAY be
            //# present in an HTTP/3 request header; when it is, it MUST NOT contain
            //# any value other than "trailers".
            if name_bytes == b"te" {
                let val_bytes = trim_ascii_whitespace(value.as_ref());
                if !val_bytes.eq_ignore_ascii_case(b"trailers") {
                    return Err(HeaderError::InvalidTeHeader);
                }
            }

            return Ok(Field::Header((
                header_name,
                HeaderValue::from_bytes(value.as_ref())
                    .map_err(|_| HeaderError::invalid_value(name, value))?,
            )));
        }

        Ok(match name {
            b":scheme" => Field::Scheme(try_value(name, value)?),
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
            //# If these fields are present, they MUST NOT be
            //# empty.
            b":authority" => Field::Authority(try_value(name, value)?),
            b":path" => Field::Path(try_value(name, value)?),
            b":method" => Field::Method(
                Method::from_bytes(value.as_ref())
                    .map_err(|_| HeaderError::invalid_value(name, value))?,
            ),
            b":status" => Field::Status(
                StatusCode::from_bytes(value.as_ref())
                    .map_err(|_| HeaderError::invalid_value(name, value))?,
            ),
            b":protocol" => Field::Protocol(try_value(name, value)?),
            //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
            //# Endpoints MUST treat a request or response that contains
            //# undefined or invalid pseudo-header fields as malformed.
            _ => return Err(HeaderError::invalid_name(name)),
        })
    }
}

fn try_value<N, V, R>(name: N, value: V) -> Result<R, HeaderError>
where
    N: AsRef<[u8]>,
    V: AsRef<[u8]>,
    R: FromStr,
{
    let (name, value) = (name.as_ref(), value.as_ref());
    let s = std::str::from_utf8(value).map_err(|_| HeaderError::invalid_value(name, value))?;
    R::from_str(s).map_err(|_| HeaderError::invalid_value(name, value))
}

/// Pseudo-header fields have the same purpose as data from the first line of HTTP/1.X,
/// but are conveyed along with other headers. For example ':method' and ':path' in a
/// request, and ':status' in a response. They must be placed before all other fields,
/// start with ':', and be lowercase.
/// See RFC7540 section 8.1.2.1. for more details.
#[derive(Debug, Default)]
#[cfg_attr(test, derive(PartialEq, Clone))]
struct Pseudo {
    //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
    //= type=implication
    //# Endpoints MUST NOT
    //# generate pseudo-header fields other than those defined in this
    //# document.

    // Request
    method: Option<Method>,
    scheme: Option<Scheme>,
    authority: Option<Authority>,
    path: Option<PathAndQuery>,

    // Response
    status: Option<StatusCode>,

    protocol: Option<Protocol>,

    len: usize,
}

#[allow(clippy::len_without_is_empty)]
impl Pseudo {
    fn request(method: Method, uri: Uri, ext: Extensions) -> Self {
        let Parts {
            scheme,
            authority,
            path_and_query,
            ..
        } = uri::Parts::from(uri);

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=implication
        //# This pseudo-header field MUST NOT be empty for "http" or "https"
        //# URIs; "http" or "https" URIs that do not contain a path component
        //# MUST include a value of / (ASCII 0x2f).
        let path = path_and_query.map_or_else(
            || PathAndQuery::from_static("/"),
            |path| {
                if path.path().is_empty() && method != Method::OPTIONS {
                    PathAndQuery::from_static("/")
                } else {
                    path
                }
            },
        );

        // If the method is connect, the `:protocol` pseudo-header MAY be defined
        //
        // See: [https://www.rfc-editor.org/rfc/rfc8441#section-4]
        let protocol = if method == Method::CONNECT {
            ext.get::<Protocol>().copied()
        } else {
            None
        };

        // For standard CONNECT (that is, without :protocol pseudo-header) scheme and path
        // are not set. See: [https://www.rfc-editor.org/rfc/rfc9114#section-4.4]
        let (scheme, path) = if method == Method::CONNECT && protocol.is_none() {
            (None, None)
        } else {
            (scheme.or(Some(Scheme::HTTPS)), Some(path))
        };

        let len = 3 + authority.is_some() as usize + protocol.is_some() as usize;

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
        //= type=implication
        //# Pseudo-header fields defined for requests MUST NOT appear
        //# in responses; pseudo-header fields defined for responses MUST NOT
        //# appear in requests.

        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=implication
        //# All HTTP/3 requests MUST include exactly one value for the :method,
        //# :scheme, and :path pseudo-header fields, unless the request is a
        //# CONNECT request; see Section 4.4.
        Self {
            method: Some(method),
            scheme,
            authority,
            path,
            status: None,
            protocol,
            len,
        }
    }

    fn response(status: StatusCode) -> Self {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
        //= type=implication
        //# Pseudo-header fields defined for requests MUST NOT appear
        //# in responses; pseudo-header fields defined for responses MUST NOT
        //# appear in requests.
        Pseudo {
            method: None,
            scheme: None,
            authority: None,
            path: None,
            status: Some(status),
            len: 1,
            protocol: None,
        }
    }

    fn len(&self) -> usize {
        self.len
    }
}

fn is_connection_specific(name: &[u8]) -> bool {
    matches!(
        name,
        b"connection" | b"keep-alive" | b"proxy-connection" | b"transfer-encoding" | b"upgrade"
    )
}

fn trim_ascii_whitespace(bytes: &[u8]) -> &[u8] {
    let mut start = 0;
    while start < bytes.len() && (bytes[start] == b' ' || bytes[start] == b'\t') {
        start += 1;
    }
    let mut end = bytes.len();
    while end > start && (bytes[end - 1] == b' ' || bytes[end - 1] == b'\t') {
        end -= 1;
    }
    &bytes[start..end]
}

#[derive(Debug)]
pub enum HeaderError {
    InvalidHeaderName(String),
    InvalidHeaderValue(String),
    InvalidRequest(http::Error),
    MissingMethod,
    MissingStatus,
    MissingAuthority,
    ContradictedAuthority,
    PseudoAfterRegularField,
    ConnectionSpecificHeader(String),
    InvalidTeHeader,
    PseudoInTrailer,
}

impl HeaderError {
    fn invalid_name<N>(name: N) -> Self
    where
        N: AsRef<[u8]>,
    {
        HeaderError::InvalidHeaderName(format!("{:?}", name.as_ref()))
    }

    fn invalid_value<N, V>(name: N, value: V) -> Self
    where
        N: AsRef<[u8]>,
        V: AsRef<[u8]>,
    {
        HeaderError::InvalidHeaderValue(format!(
            "{:?} {:?}",
            String::from_utf8_lossy(name.as_ref()),
            value.as_ref()
        ))
    }
}

impl std::error::Error for HeaderError {}

impl fmt::Display for HeaderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HeaderError::InvalidHeaderName(h) => write!(f, "invalid header name: {}", h),
            HeaderError::InvalidHeaderValue(v) => write!(f, "invalid header value: {}", v),
            HeaderError::InvalidRequest(r) => write!(f, "invalid request: {}", r),
            HeaderError::MissingMethod => write!(f, "missing method in request headers"),
            HeaderError::MissingStatus => write!(f, "missing status in response headers"),
            HeaderError::MissingAuthority => write!(f, "missing authority"),
            HeaderError::ContradictedAuthority => {
                write!(f, "uri and authority field are in contradiction")
            }
            HeaderError::PseudoAfterRegularField => {
                write!(
                    f,
                    "pseudo-header field appears after a regular header field"
                )
            }
            HeaderError::ConnectionSpecificHeader(h) => {
                write!(f, "connection-specific header not permitted: {}", h)
            }
            HeaderError::InvalidTeHeader => {
                write!(f, "te header field may only contain 'trailers'")
            }
            HeaderError::PseudoInTrailer => {
                write!(f, "pseudo-header field appears in trailer section")
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    #[test]
    fn request_has_no_authority_nor_host() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=test
        //# If the :scheme pseudo-header field identifies a scheme that has a
        //# mandatory authority component (including "http" and "https"), the
        //# request MUST contain either an :authority pseudo-header field or a
        //# Host header field.
        let headers = Header::try_from(vec![(b":method", Method::GET.as_str()).into()]).unwrap();
        assert!(headers.pseudo.authority.is_none());
        assert_matches!(
            headers.into_request_parts(),
            Err(HeaderError::MissingAuthority)
        );
    }

    #[test]
    fn request_has_empty_authority() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=test
        //# If these fields are present, they MUST NOT be
        //# empty.
        assert_matches!(
            Header::try_from(vec![
                (b":method", Method::GET.as_str()).into(),
                (b":authority", b"").into(),
            ]),
            Err(HeaderError::InvalidHeaderValue(_))
        );
    }

    #[test]
    fn request_has_empty_host() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=test
        //# If these fields are present, they MUST NOT be
        //# empty.
        let headers = Header::try_from(vec![
            (b":method", Method::GET.as_str()).into(),
            (b"host", b"").into(),
        ])
        .unwrap();
        assert_matches!(
            headers.into_request_parts(),
            Err(HeaderError::InvalidRequest(_))
        );
    }

    #[test]
    fn request_has_authority() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=test
        //# If the :scheme pseudo-header field identifies a scheme that has a
        //# mandatory authority component (including "http" and "https"), the
        //# request MUST contain either an :authority pseudo-header field or a
        //# Host header field.
        let headers = Header::try_from(vec![
            (b":method", Method::GET.as_str()).into(),
            (b":authority", b"test.com").into(),
        ])
        .unwrap();
        assert_matches!(headers.into_request_parts(), Ok(_));
    }

    #[test]
    fn request_has_host() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=test
        //# If the :scheme pseudo-header field identifies a scheme that has a
        //# mandatory authority component (including "http" and "https"), the
        //# request MUST contain either an :authority pseudo-header field or a
        //# Host header field.
        let headers = Header::try_from(vec![
            (b":method", Method::GET.as_str()).into(),
            (b"host", b"test.com").into(),
        ])
        .unwrap();
        assert!(headers.pseudo.authority.is_none());
        assert_matches!(headers.into_request_parts(), Ok(_));
    }

    #[test]
    fn request_has_same_host_and_authority() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=test
        //# If both fields are present, they MUST contain the same value.
        let headers = Header::try_from(vec![
            (b":method", Method::GET.as_str()).into(),
            (b":authority", b"test.com").into(),
            (b"host", b"test.com").into(),
        ])
        .unwrap();
        assert_matches!(headers.into_request_parts(), Ok(_));
    }
    #[test]
    fn request_has_different_host_and_authority() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3.1
        //= type=test
        //# If both fields are present, they MUST contain the same value.
        let headers = Header::try_from(vec![
            (b":method", Method::GET.as_str()).into(),
            (b":authority", b"authority.com").into(),
            (b"host", b"host.com").into(),
        ])
        .unwrap();
        assert_matches!(
            headers.into_request_parts(),
            Err(HeaderError::ContradictedAuthority)
        );
    }

    #[test]
    fn preserves_duplicate_headers() {
        let headers = Header::try_from(vec![
            (b":method", Method::GET.as_str()).into(),
            (b":authority", b"test.com").into(),
            (b"set-cookie", b"foo=foo").into(),
            (b"set-cookie", b"bar=bar").into(),
            (b"other-header", b"other-header-value").into(),
        ])
        .unwrap();

        assert_eq!(
            headers
                .clone()
                .into_iter()
                .filter(|h| h.name.as_ref() == b"set-cookie")
                .collect::<Vec<_>>(),
            vec![
                HeaderField {
                    name: std::borrow::Cow::Borrowed(b"set-cookie"),
                    value: std::borrow::Cow::Borrowed(b"foo=foo")
                },
                HeaderField {
                    name: std::borrow::Cow::Borrowed(b"set-cookie"),
                    value: std::borrow::Cow::Borrowed(b"bar=bar")
                }
            ]
        );
        assert_eq!(
            headers
                .into_iter()
                .filter(|h| h.name.as_ref() == b"other-header")
                .collect::<Vec<_>>(),
            vec![HeaderField {
                name: std::borrow::Cow::Borrowed(b"other-header"),
                value: std::borrow::Cow::Borrowed(b"other-header-value")
            },]
        );
    }

    #[test]
    fn rejects_undefined_pseudo_header() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
        //= type=test
        //# Endpoints MUST treat a request or response that contains
        //# undefined or invalid pseudo-header fields as malformed.
        assert_matches!(
            Header::try_from(vec![(b":unknown", b"value").into()]),
            Err(HeaderError::InvalidHeaderName(_))
        );
    }

    #[test]
    fn rejects_pseudo_header_after_regular_header() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
        //= type=test
        //# Any request or response that contains a
        //# pseudo-header field that appears in a header section after a regular
        //# header field MUST be treated as malformed.
        assert_matches!(
            Header::try_from(vec![
                (b"regular", b"value").into(),
                (b":method", b"GET").into(),
            ]),
            Err(HeaderError::PseudoAfterRegularField)
        );
    }

    #[test]
    fn rejects_connection_specific_headers() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //= type=test
        //# An endpoint MUST NOT generate
        //# an HTTP/3 field section containing connection-specific fields; any
        //# message containing connection-specific fields MUST be treated as
        //# malformed.
        for name in &[
            "connection",
            "keep-alive",
            "proxy-connection",
            "transfer-encoding",
            "upgrade",
        ] {
            assert_matches!(
                Header::try_from(vec![
                    (b":method", b"GET").into(),
                    (b":scheme", b"https").into(),
                    (b":authority", b"example.com").into(),
                    (b":path", b"/").into(),
                    (name.as_bytes(), b"foo").into(),
                ]),
                Err(HeaderError::ConnectionSpecificHeader(_))
            );

            let mut map = HeaderMap::new();
            map.insert(
                HeaderName::from_static(name),
                HeaderValue::from_static("foo"),
            );
            assert_matches!(
                Header::request(
                    Method::GET,
                    Uri::from_static("https://example.com/"),
                    map.clone(),
                    Extensions::new()
                ),
                Err(HeaderError::ConnectionSpecificHeader(_))
            );
            assert_matches!(
                Header::response(StatusCode::OK, map.clone()),
                Err(HeaderError::ConnectionSpecificHeader(_))
            );
            assert_matches!(
                Header::trailer(map),
                Err(HeaderError::ConnectionSpecificHeader(_))
            );
        }
    }

    #[test]
    fn validates_te_header() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2
        //= type=test
        //# The only exception to this is the TE header field, which MAY be
        //# present in an HTTP/3 request header; when it is, it MUST NOT contain
        //# any value other than "trailers".

        // Valid: "trailers" in request
        assert!(Header::try_from(vec![
            (b":method", b"GET").into(),
            (b":scheme", b"https").into(),
            (b":authority", b"example.com").into(),
            (b":path", b"/").into(),
            (b"te", b"trailers").into(),
        ])
        .is_ok());

        // Valid with whitespace and mixed casing
        assert!(Header::try_from(vec![
            (b":method", b"GET").into(),
            (b":scheme", b"https").into(),
            (b":authority", b"example.com").into(),
            (b":path", b"/").into(),
            (b"te", b" Trailers ").into(),
        ])
        .is_ok());

        // Invalid: TE with value other than trailers in request
        assert_matches!(
            Header::try_from(vec![
                (b":method", b"GET").into(),
                (b":scheme", b"https").into(),
                (b":authority", b"example.com").into(),
                (b":path", b"/").into(),
                (b"te", b"gzip").into(),
            ]),
            Err(HeaderError::InvalidTeHeader)
        );

        // Invalid: TE in response
        let header = Header::try_from(vec![
            (b":status", b"200").into(),
            (b"te", b"trailers").into(),
        ])
        .unwrap();
        assert_matches!(
            header.into_response_parts(),
            Err(HeaderError::ConnectionSpecificHeader(_))
        );

        // Invalid: TE in trailers
        let header = Header::try_from(vec![(b"te", b"trailers").into()]).unwrap();
        assert_matches!(
            header.into_trailer_parts(),
            Err(HeaderError::ConnectionSpecificHeader(_))
        );
    }

    #[test]
    fn concatenates_multiple_cookie_headers() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.2.1
        //= type=test
        //# If a decompressed field
        //# section contains multiple cookie field lines, these MUST be
        //# concatenated into a single byte string using the two-byte delimiter
        //# of "; " (ASCII 0x3b, 0x20) before being passed into a context other
        //# than HTTP/2 or HTTP/3, such as an HTTP/1.1 connection, or a generic
        //# HTTP server application.
        let header = Header::try_from(vec![
            (b":method", b"GET").into(),
            (b":scheme", b"https").into(),
            (b":authority", b"example.com").into(),
            (b":path", b"/").into(),
            (b"cookie", b"a=b").into(),
            (b"cookie", b"c=d").into(),
            (b"cookie", b"e=f").into(),
        ])
        .unwrap();

        let (_, _, _, fields) = header.into_request_parts().unwrap();
        assert_eq!(fields.get(header::COOKIE).unwrap(), "a=b; c=d; e=f");
    }

    #[test]
    fn rejects_pseudo_headers_in_trailers() {
        //= https://www.rfc-editor.org/rfc/rfc9114#section-4.3
        //= type=test
        //# Pseudo-header fields MUST NOT appear in trailer
        //# sections.
        let header = Header::try_from(vec![
            (b":status", b"200").into(),
            (b"some-trailer", b"value").into(),
        ])
        .unwrap();
        assert_matches!(
            header.into_trailer_parts(),
            Err(HeaderError::PseudoInTrailer)
        );
    }
}
