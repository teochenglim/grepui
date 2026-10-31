# Guides

AsciiDoc, so GitHub renders them and `asciidoctor` can make HTML or PDF.

- [User Guide](user-guide.adoc): every page of the UI for the people who use it, with screenshots.
- [Technical Guide](technical-guide.adoc): install, every setting, orgs and roles, single sign-on, production notes, security, the API, troubleshooting.

Screenshots live in [../../images/](../../images/) and are referenced by name. `tests/ac_docs.rs` fails if a guide or the README names an image that does not exist, if a page of the UI is missing from the User Guide, or if a flag, environment variable, command or route is missing from or wrong in the Technical Guide.
