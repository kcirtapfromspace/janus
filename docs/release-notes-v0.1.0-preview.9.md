# Interview Coach 0.1.0 preview 9 — Switching Claude accounts works

## The problem

Switching to another Claude account could fail on the approval page with "You don't have access to this organization or it doesn't meet the requirements for Anthropic CLI."

Interview Coach's sign-in profile remembered the organization and workspace of the first account. Signing in again asked the approval page for that same organization, which a different account isn't a member of.

## The fix

- **Signing out forgets the organization too.** That covers Switch Account… and `ic logout`.
- **Every fresh sign-in clears a leftover organization first.** A Mac already stuck in this state recovers too: open Setup and sign in again.
- **You pick the account.** The approval page now lets you choose the account and organization.

If the organization you pick still isn't allowed, it may be one that can't use the Anthropic CLI. A Team or Enterprise organization may need its admin to allow it. The error message now says so.

## Validation

- **Tests:** 122 Rust unit tests, 21 analysis and stage tests, and 32 Swift tests pass, and clippy is clean.
- **End to end, in a scratch copy of the sign-in settings:**
  - `ic logout` removed the sign-in and the remembered organization;
  - the next approval link no longer named an organization;
  - before the fix, it named the old one.
- **Not done:** approving in the browser with a second account. That needs you.
