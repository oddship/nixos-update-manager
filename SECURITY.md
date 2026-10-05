# Security reporting

This project is a development preview. There is no supported stable release or security support window yet. Do not use it as a production updater while release validation is incomplete.

Report vulnerabilities privately through [GitHub private vulnerability reporting](https://github.com/oddship/nixos-update-manager/security/advisories/new). If that service is unavailable, contact a maintainer through a private channel listed on their GitHub profile. Do not post exploit details, secrets, private configuration contents, or credentials in a public issue.

Include the affected revision, a minimal synthetic reproducer, the expected authority boundary, and the observed effect. Pay particular attention to wrong-candidate activation, checkout/index corruption, root-helper argument handling, state recovery, source isolation, and disclosure through logs or future agent integrations.

There is no published response-time guarantee. Maintainers should acknowledge reports, reproduce them in disposable guests, agree on disclosure timing with the reporter, and publish an advisory with affected revisions and remediation when appropriate.
