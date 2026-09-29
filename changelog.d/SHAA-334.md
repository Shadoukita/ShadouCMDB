### Security: the identity-provider connection test no longer reveals why a connection failed

`POST /api/v1/admin/identity-providers/{id}/test` returned the raw connection or TLS error, so an
administrator session could use it to tell closed, filtered and non-TLS ports on internal hosts apart
([GH#125]). When no answer came back over verified TLS (connection refused or timed out, TLS or
StartTLS failed, not an LDAP server), the test now shows the same generic message whatever the cause,
and the exact error goes to the server log (`identity provider connection test failed`). Answers from
a provider over verified TLS (HTTP status, issuer mismatch, LDAP result codes) are still shown, since
they are needed to fix the settings. See [enterprise sign-in].

**Upgrade:** nothing to do. To troubleshoot a failing test, read the server log instead of the test
result.

[GH#125]: https://github.com/Shadoukita/ShadouCMDB/issues/125
[enterprise sign-in]: docs/api.md#enterprise-sign-in
