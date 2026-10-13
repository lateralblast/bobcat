# Issues

| ID | Status | Description | Fixed in |
|----|--------|-------------|----------|
| 1 | Fixed | SSH host key is not verified (also in the GUI SSH console) (iDRAC certs are self-signed); vulnerable to MITM | 0.9.6 (pinned on first use) |
| 2 | Fixed | GUI TLS mode did not verify the iDRAC's (self-signed) certificate; vulnerable to MITM | 0.5.0 (pinned on first use) |
