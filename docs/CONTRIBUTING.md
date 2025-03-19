Thank you for the interest in contributing to `h3`. All important information are below.

# Duvet
The [`duvet`][] crate is used in `h3` to track [spec][] compliance.
`duvet` does that via spec citations in the code, as customized comments.
The comments should be kept up-to-date with the code.

[duvet]: https://crates.io/crates/duvet

## General
Duvet tracks all keywords (MUST, MUST NOT, SHOULD, SHOULD NOT and MAY) from an RFC.  
You can see those in the [report][] on the menu on the left side. There are all sections of the [spec][]. On each section the paragraphs with the keywords are highlighted.  

Duvet sees all citations of each of these paragraphs and their status. At the bottom of each section there is a summary of all the keywords with their status.


[spec]: https://www.rfc-editor.org/rfc/rfc9114
[report]: https://hyper.rs/h3/ci/compliance/report.html#/

## Citations

There are three ways to keep track of all the requirements: 

1. In line citations as comments in the source code
2. Citations in .toml files
3. Issues in the repository


The citations can be created with a click on the highlighted sections in the sections.  
There are different citation types with different meanings.  
### Citation
Standard citation in the source code where the requirement is implemented.

### Implication
Citation where the related code is self-evident. This means no Test is required.

### Test
Indicates that the related code is a test for the requirement.

### Exception 
Is the marker that the requirement is not applied to the crate. For example those which are applied to IANA registration. 

### Todo
Is the marker that the requirement still needs to be fulfilled. 
