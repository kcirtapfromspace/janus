# The shared question registry

A Cloudflare Worker in front of a D1 database, both on the free tier. Janus installs whose owners
opt in (Settings › Share my interview questions) send interview questions there. Janus rewrites
each question on the Mac first, so it names no person, company or product. Company names are
only included if the contributor also turns that on, and they are never published.

Contributions stay private. Only the maintainer reads them, from their own Mac with `wrangler`;
the Worker has no admin route. Approved questions are what every Janus downloads for practice
interviews (`GET /v1/questions`). Anyone with the URL can read them, and every copy of the app
contains the URL, so they must never identify anyone.

## Moderating

From a Mac where `wrangler` is signed in to the maintainer's Cloudflare account:

```sh
ic registry pending                 # what's waiting
ic registry approve 12 14 15        # publish them (merged into an existing question when it's the same)
ic registry approve 16 --as "Tell me about a time you had to push back on a deadline."
ic registry reject 13
```

## Withdrawing

`DELETE /v1/contributions` with an install's id removes everything that install sent and hasn't
had approved. Janus does this when you turn sharing off and choose to withdraw, or with
`ic registry withdraw`. A question already approved stays, because it is generic and other people
may have contributed it too.

## Setting it up

```sh
cd cloud/registry
wrangler d1 create janus-registry       # put the database_id it prints into wrangler.toml
wrangler d1 execute janus-registry --remote --file schema.sql
wrangler deploy                          # prints the URL; it's Janus's registry_url default
```

Local development: `wrangler d1 execute janus-registry --local --file schema.sql`, then
`wrangler dev --local`, and point Janus at it with `IC_REGISTRY_URL=http://127.0.0.1:8787`.
