# Security policy

Rescyo parses untrusted disk images and often runs with administrator or root privileges, so memory-safety and logic bugs in parsers can be security issues. Two kinds of bugs are treated as security issues:

- a crafted disk or image that makes Rescyo crash, hang, exhaust memory, or execute code;
- any path by which Rescyo writes to a Source device, or writes outside the chosen Target.

## Reporting

Report privately through [GitHub Security Advisories](https://github.com/akk0sfx/Rescyo/security/advisories/new). Include a minimal image or byte sequence if you can. Please do not include anyone's real personal data.

We aim to acknowledge reports within 7 days. Until v1.0, fixes ship in the next release from `main`.

## Supported versions

Only the latest release is supported.
