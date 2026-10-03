# OAuth test fixtures

`test-key.pem` is a newly generated, deliberately public test-only RSA private key. It has no
connection to an OpenAI account or production signing identity. `jwks.json` contains its public
key. Unit tests use them to verify signature, issuer, audience, expiration and nonce rejection.
Never use this key outside tests.
