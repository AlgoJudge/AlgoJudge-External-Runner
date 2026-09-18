## What changes and why

<!-- One subject per pull request. If it closes an issue, write "Closes #123". -->

## How it was tested

<!-- The commands you ran, and the submissions you forwarded by hand. -->

## Checklist

- [ ] `./x gate` passes. It runs formatting, Clippy, the build, and the tests the way CI does.
- [ ] A new integration is a module beside `src/uva/` that implements the trait in `src/integration.rs`.
- [ ] A new configuration key is in `.env.example`.
- [ ] No secrets, credentials, judging-system accounts, or `.env` files are committed.
