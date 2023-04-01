const URLS = ["https://127.0.0.1:*/", "https://echo.webtransport.day:*/"];

chrome.webRequest.onBeforeRequest.addListener(
  function(details) {
    console.log("Request intercepted:", details);
    console.log("Request extraHeaders:", details.extraHeaders);
    console.log("Request requestBody:", details.requestBody);
  },
  {urls: URLS},
  ["extraHeaders", "requestBody"]
);

chrome.webRequest.onBeforeSendHeaders.addListener(
  function(details) {
    console.log("onBeforeSendHeaders:", details);
    console.log("onBeforeSendHeaders extraHeaders:", details.extraHeaders);
  },
  {urls: URLS},
  ["requestHeaders"]
);

chrome.webRequest.onSendHeaders.addListener(
  function(details) {
    console.log("onSendHeaders:", details);
    console.log("onSendHeaders requestHeaders:", details.requestHeaders);
  },
  {urls: URLS},
  ["requestHeaders"]
);


chrome.webRequest.onHeadersReceived.addListener(
  function(details) {
    console.log("onHeadersReceived:", details);
    console.log("onHeadersReceived responseHeaders:", details.responseHeaders);
  },
  {urls: URLS},
  ["responseHeaders"]
);


chrome.webRequest.onResponseStarted.addListener(
  function(details) {
    console.log("onResponseStarted:", details);
    console.log("onResponseStarted responseHeaders:", details.responseHeaders);  
  },
  {urls: URLS},
  ["responseHeaders"]
);

chrome.webRequest.onCompleted.addListener(
  function(details) {
    console.log("onCompleted:", details);
    console.log("onCompleted responseHeaders:", details.responseHeaders);
  },
  {urls: URLS},
  ["responseHeaders"]
);


chrome.webRequest.onErrorOccurred.addListener(
  function(details) {
    console.log("onErrorOccurred:", details);
  },
  {urls: URLS},
  []
);
