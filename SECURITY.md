# Security Policy

## Supported Versions

Security fixes are applied to the latest published release.

## Reporting a Vulnerability

Please use GitHub's private vulnerability reporting for this repository. Do not
open a public issue for an unpatched vulnerability and do not include secrets,
private voice recordings, or personal data in a report.

Include the affected version, reproduction steps, impact, and any suggested
mitigation. You should receive an initial response within seven days.

## Security Boundaries

soundAr runs model code and processes local audio on the user's machine. Model
weights and Python packages are third-party software and should only be obtained
from sources the user trusts. Voice cloning must only be performed with consent
and with audio the user is authorized to process.
