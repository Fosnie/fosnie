# OOXML schemas — provenance

These are the **ISO/IEC 29500-4 (Transitional)** Office Open XML XML Schemas (the
ECMA-376 / OOXML 2006 namespaces), plus the W3C `xml.xsd`, the Markup-Compatibility
(`mce/`), OPC (`ecma/`) and Microsoft extension (`microsoft/`) schemas the WML schema
imports. They are the freely-redistributable standard schemas published by ISO/IEC
and ECMA International.

Used by `ml/app/validators.py::validate_docx_xsd` to validate generated DOCX
(`word/document.xml`) against `ISO-IEC29500-4_2016/wml.xsd` after stripping
markup-compatibility / non-OOXML extension namespaces (the validation *code* is our
own re-implementation — only these standard schema files are vendored).

Vendored once at build time (zero-egress at runtime). The directory is exempt from
the no-external-URL artefact scan (the schemas legitimately reference the standard
namespace URIs).
