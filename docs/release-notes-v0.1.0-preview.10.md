# Interview Coach 0.1.0 preview 10 — Organize your interviews

## Companies, roles and rounds

The sidebar is now a tree.
- **Companies:** each company you're interviewing with is a section.
- **Roles:** under each company, the roles you're applying for, each with a status chip (Interviewing, Offer, Accepted, Rejected or Withdrawn).
- **Rounds:** under each role, its interviews in date order, labelled by round (recruiter screen, technical, hiring manager…).
- **Unfiled:** interviews without a role sit under their company, and ones without a company go under "No company".

**Filing is automatic, once.** A new report files its interview under the company and role it detected. Your interviews were filed the same way when you updated. After that, the interview stays where it is: a re-run with another model that names the role differently doesn't move it, and neither does one you've moved or taken out of its role yourself. The round a report detects is kept only when none is set, so a round you've set stays.

## Managing them

Right-click an interview, or several (Cmd- or Shift-click to select more):
- **Edit Details…** sets the title, company, role and round. The report reads the title and the company you enter, so after changing either, re-running the report makes a new version.
- **Move to Role**: a role at that company, a new role, or no role.
- **Archive** hides an interview from the main list. "Show archived", at the bottom of the sidebar, brings archived ones back into view.
- **Delete** moves it to **Recently Deleted**, at the bottom of the sidebar.

Recently Deleted keeps an interview for 30 days, then erases its recording, transcript and reports for good. Until then, **Restore** brings it back. A deleted interview still opens, with a banner showing how long it has left. **Erase Now…** and **Empty Recently Deleted…** erase straight away, after a confirmation. Erasing only removes the interview's own folder in Interview Coach's data folder, never an original file you imported.

Right-click a **role** to:
- set its status;
- rename it;
- merge it into another role at the same company, for the duplicates two models' titles can create;
- archive it.

Right-click a **company** to rename it everywhere or to archive it: its roles, and its interviews that aren't under a role. An interview added later for an archived company shows up normally.

Renaming a company has two side effects:
- **New versions:** it changes the company on each interview you entered it for, so re-running any of those reports makes a new version, as with Edit Details.
- **Separate heading:** an interview whose company only came from its report (no role, nothing entered) keeps the old name and shows under its own heading until you move it to a role or edit it.

You can't archive or delete an interview while it's recording, transcribing or being analysed.

## Search and filters

- **Search:** the field at the top of the sidebar searches titles, companies, roles, and everything said in your transcripts.
- **Filters:** the menu at the bottom filters by outcome, verdict, or where the role stands.

## In Terminal

- `ic edit <id> --title … --company … --role … --round …`
- `ic archive`, `ic delete`, `ic restore`, `ic erase`, `ic empty-deleted`
- `ic role list|status|rename|archive|merge`
- `ic company rename|archive`
- `ic search <text>`
- `ic list --all` (archived and deleted too)

## Validation

- **Tests:** 125 Rust unit tests, 28 integration tests (7 of them new for filing, archive, Recently Deleted, erasing and search), and 35 Swift tests pass. Clippy is clean.
- **On a copy of your data:**
  - the upgrade filed your interview under "ML Platform Engineer" at Agility;
  - archive, restore, delete, edit round and search all behaved as expected;
  - search found a transcript line at 00:20:39.
- **The sidebar, Edit Details and the multi-select view** were checked with the offscreen renderer. Clicking through them, and right-click menus in particular, hasn't been done in the released app.
