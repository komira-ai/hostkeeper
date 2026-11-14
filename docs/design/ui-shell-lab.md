# The coordinator's web UI: a new frame, and Home as the lab

Status: built on `ui-shell-lab-home` (komira-ai/hostkeeper). Owner direction of 2026-11-13: "this whole ui needs to look better ... a side
menu is more modern ... use our komira colour palette"; the Fleet, Release, Rollout and Holds entries leave the menu; the
"hostkeeper" wordmark goes Home; Home is an overhead view of the lab with rows, a click on a row shows its racks, a click on a host
opens the host page; and "when enrolling a host, that is where we enter row and rack position and U position".

Release, rollout and holds move into the package detail page, which another change builds (`packages-page`, `package-detail`). This
change keeps their routes open and takes them out of the menu; that change turns them into redirects to the package.

## 1. What the operator sees

Before: a top bar of twelve buttons (Fleet, Host, Rollout, Host groups, Release, Holds, Enrollment, Inventory, Audit, Users, API tokens,
MDM setup), Fleet a table of hosts. After: a side menu, and Home a picture of the lab.

```
+-----------+-------------------------------------------------------------+
| hostkeeper|  Lab / Row 2                         user  Theme  Sign out  |   top bar (breadcrumb of the page)
| MANAGE    +-------------------------------------------------------------+
|  Host gr. |  Lab 1                                       [Edit layout]  |
|  Inventory|  healthy 12  updating 1  failed 0  offline 0  pending 2      |
|  Enrollm. |  +-- floor plan ---------------------------------------+    |
| ADMIN     |  | ROW 1  [1 12/42U] [2 30/42U] [3 0/42U]   42 servers |    |   a click opens the row
|  Users    |  | ROW 2  [1 40/42U] [2 18/42U]             19 servers |    |
|  Tokens   |  +-----------------------------------------------------+    |
|  MDM      |  Unplaced (2)  [new-1 pending] [web-9 healthy]              |
|  Audit    |  Alerts: one compact strip above the floor plan              |
+-----------+-------------------------------------------------------------+
```

- Menu (wide, 232 px; collapsed to 56 px icons by a button; a drawer under 640 px): Host groups, Inventory, Enrollment, then, at the bottom, Settings
  (tabs: Users, API tokens, MDM, Audit), the theme toggle, Sign out and Collapse. MDM is a setting, not a page of the daily work. The Packages entry arrives with the Packages page (a line of `GROUPS` in `ui/app.js`).
  No Fleet, Release, Rollout or Holds entry. The wordmark at the top goes Home. The top bar shows where the operator is, the user,
  the theme toggle (browser, light, dark; in memory only) and Sign out.
- Home, three levels of one page. **Floor plan**: a band for each row of the cluster, each rack a footprint (number, U in use as a
  fill, one counter per state), hover glows, a click opens the row. **Row**: the row's racks as front elevations side by side in a
  horizontally scrolling strip (wheel, drag, arrow keys; scroll-snap per rack; the rack number above each, a button to open that
  rack alone): a dark cabinet with U numbers on the rail, each server a faceplate over its U with bezel detail, a light (green
  healthy, amber updating with a slow pulse, solid red failed, a hollow red ring offline, grey pending), its name and its state in words, a blank panel over every free
  run of U; hover says name, group, version, state; a click opens the host page. **Rack**: one rack larger, and a table of its
  servers. Breadcrumbs `Lab > Row > Rack > Host` (none on the floor plan itself). Above the floor plan: the alerts as one compact strip
  (toned by the worst alert; "No alerts" when there are none). Below it: the servers not placed, a quiet line of outlined chips. A
  Floor / List switch (`list` in the page's params) shows every server in one compact list (server, state, where, group, version) in
  place of the floor plan; the Fleet page's table of hosts is gone from Home. A rack footprint is a small elevation (a block per
  server in the colour of its state); the strip of racks shows "Rack n of m" for the rack at its left edge, two arrow buttons, fades
  at an end with more beyond, snaps to a rack, and fits a rack's height to the viewport; a phone shows one rack at a time.
- **Host page** (`ui/pages/host.js`, `ui/styles/host.css`): a dark status header (name, a light and the state in words, role,
  group, what it runs, the last heartbeat, and a small elevation of the rack the host stands in with its U lit, or "Unplaced"), then
  tabs (Overview, Services, Certificate, Labels, Placement, Host group; a dot on Services or Certificate when something needs
  attention; the tab chosen is kept across a refresh of the same host), then Retire alone in a red-bordered danger zone at the bottom.
  The state is `degraded`, not `healthy`, for a live host whose service reports unhealthy or whose last update failed.
- Edit layout (a toggle on Home) shows the controls: rename the cluster, add a row (the next number), add a rack to a row (height
  from a list, 42 U by default), resize, move to another row, remove; move a server by picking it up (or dragging it)
  and putting it on a free U, or taking it out of its rack. A row or rack that still holds servers is not removed: the page names
  them. A refusal (`overlap`, `out_of_rack`, ...) is told in words. An empty lab says so and offers "Add a row".
- Enrollment: the approval form gets the Height (1U to 4U, 1U by default, chosen before the U position), the Row (a dropdown of the
  rows, first choice "Unplaced"), the Rack (that row's racks) and the U position (the free slots of the rack for that height). No
  free text. Placement is optional; an approval with none lands the host in Unplaced. Approve stores the placement in the same step.
  Each pending request is one compact card (`ui/styles/enrollment.css`): the inventory match, one small table of the key facts (a
  renewal or a nonce only when there is one), then one row of controls (Host id, Host group with "No group", Height, Row, Rack, U
  position) with Approve and Reject; the explanations are small info marks, a contested or duplicate request a chip and a quiet
  line, and "not recorded" is grey.
- Host: a Placement section says where the server stands and has the same four choices (Place or Move) and Unplace.
- Not changed: every other page, every existing endpoint, the sign-in. Release, Rollout and Holds open by their route.

Look: the komira palette as CSS tokens (`ui/app.css`: the ink `#0B0B0A`, the accent `#FD4F1D`, a dark slate for the menu and for dark
mode), system fonts, light and dark by `prefers-color-scheme` and the toggle; the lab hardware (`ui/lab.css`) stays graphite in both
themes with thin accent lines and a glow on hover. No image, no web font, no runtime CDN, no build step; the layout is HTML and
CSS grid, a position handed to the stylesheet as a custom property (`--r`, `--s`) set through the element's style object, which the
content security policy `default-src 'self'` allows (a `style` attribute it does not). `prefers-reduced-motion` turns the pulse and
the smooth scroll off. A state is never a colour alone: the pill, the plate and the tooltip carry its word.

## 2. Data model (the coordinator)

The location of a server is not an inventory field: the inventory keeps its four (name, BMC MAC, data MAC, notes). The lab is a small
model of its own, one lab only:

- the cluster's name (1 to 32 characters of letters, digits, space, `.`, `_`, `-`);
- rows `{id: row-<n>, number}` numbered 0, 1, 2, ... by the coordinator;
- racks `{id: rack-<n>, row, number, height_u}`, numbered within their row, 1 to 60 U high;
- placements `{subject {kind HOST | INVENTORY, id}, rack, u, height_u}`: the subject occupies U `u` to `u + height_u - 1`, U 1 the bottom;
  at most one placement for a subject, none shares a U with another of its rack.

The place belongs to the physical server, so it survives a wipe and a new enrollment: the subject is the inventory entry when one matches
the host, else the host. Limits: 64 rows, 512 racks, 4096 placements. One function judges the whole layout (`Layout::new`), every
change is made on a copy and judged whole, so a rule is stated once. A stored layout that breaks a rule stops the start, naming the file.

Storage, additive (B8): a store part beside the accounts. File store: `<state-file>.layout` (JSON, mode 0600, written whole when a commit
changes it). Postgres: migration `0015_lab_layout.sql` (`may-run`): `lab_cluster`, `lab_rows`, `lab_racks`, `lab_placements`. Backup and
restore carry the file, `export` writes the layout file from the tables, `counts.sql` lists the tables. An older release never reads
them; a coordinator that finds none starts with an empty layout.

Network devices (switches) are part of this model too: `docs/design/network-gear.md`.

## 3. Operator API

New service `Layout` (`layout.proto`), operator token only: `Get`, `PutName`, `AddRow`, `RemoveRow` (`in_use` while it has racks), `PutRack`
(add, resize, move), `RemoveRack` (`in_use` while it holds placements), `Place` (put or move), `Unplace`. Each write takes an optional
reason, is audited first (`layout_name_put`, `layout_row_add`, `layout_row_remove`, `layout_rack_put`, `layout_rack_remove`,
`layout_place`, `layout_unplace`), and is refused whole with nothing changed and no audit line: `overlap`, `out_of_rack`, `unknown_rack`,
`unknown_row`, `unknown_placement`, `in_use`, `capacity`, `invalid_request`, plus the usual `unauthorized`, `forbidden`,
`store_failed`, `audit_unavailable`.

`Enrollment.Approve` gains three optional fields (`rack` 9, `placement_u` 10, `placement_height_u` 11): the placement is made in the same
step as the approval. Both are judged first under the layout's lock, so a placement that cannot be made refuses the approval
(`placement_invalid`, `unknown_rack`, `placement_overlap`, `placement_out_of_rack`; four new values of `EnrollmentRefusal`, 27 to 30) and
nothing changes; the placement is of the inventory entry the approved host id names, else of the host, audited as `layout_place`
with the reason `approval`. A placement the store then cannot write leaves the approval standing, logged: place the host on Home.

Compatibility (B8): only additions: a new file, a new service, three optional fields, four enum values, new tables, a new file beside
the state. Nothing is renumbered or required. The test `crates/hostkeeper-proto/tests/additions_lab_layout.rs` runs the release before on
the new messages (it keeps every byte, and an `ApproveRequest` carrying a placement) and this build on what the release before wrote (an
approval asks for no placement); `hkctl` gets the verbs from the generated command table and `ui/gen/` from the generator.

## 4. The frame in the code

- `ui/app.js`: `GROUPS` (the menu), `LINKED` (pages with no entry: Home, Host, the three release pages), `ROUTES` (an old route and the page
  it lands on: `fleet` is `home`; `navigate(old, params)` resolves through it), the frame (side, scrim, top bar, collapse, drawer, theme
  port `prefs.apply`).
- `ui/lab.js`: the model the views share (layout, servers joined with hosts and inventory entries, states, free U for a height,
  breadcrumbs, the placement choices, the words of a refusal). `ui/pages/home.js`: the three levels and the edit mode.
  `ui/pages/enrollment.js`: the placement controls only; `ui/pages/host.js` also lays out the Host page above.
- `ui/view.js`: `pill`, the `vars` of an element (custom properties), `aria-*`, `draggable`, `tabindex`, and the events the strip and the
  drag use. `ui/app.css` (tokens, frame, components) and `ui/lab.css` (the lab) are two files so that each stays under 1000 lines.
  `ui/styles/forms.css` holds the page-scoped styles of sign-in, Inventory, Host groups and MDM setup (each rule starts with its page's class).

## 5. Tests

- Coordinator: the layout rules (names, heights, numbering, overlap, boundaries U 1 and the top U, capacity, ordering, file round trip,
  a bad file refused by name, records read as strictly), the store (commit before memory, a failing backend), Fake and Postgres
  conformance, backup and restore, export, the operator calls (each answer and audit entry, every refusal with nothing changed, only the
  operator, a failing store or audit, the gRPC status of each refusal), and the approval with a placement (host, inventory entry, every
  refusal leaves the request pending).
- Wire: `additions_lab_layout.rs` above; the real router test serves each `Layout` call to the operator's token over gRPC and the gateway.
- UI (a JavaScript engine against a fake gateway): the shell (menu, wordmark, collapse, drawer, theme, user and sign-out), old routes
  (`fleet`, and Release, Rollout, Holds still open), the lab at each level (floor plan, row, rack, breadcrumbs, empty, gone, failed
  layout, the strip's keys and drag), the edit mode (each call and body, refusals in words, names, pick, click and drag to place, take
  out), the placement on the approval and on the Host page (choices that fit, calls, refusals), and the mounting of custom properties.
- Report: `docs/report/hostkeeper.html` draws the shell (wide and narrow, light and dark), the lab at each level and the edit mode, and the
  placement fields of the approval and the Host page; its test checks each UI string against the page modules.

## 6. Decisions

- Placement attached to the inventory entry, not to the enrollment record, so a wipe keeps the place; a host no entry matches carries its own.
- Layout ids are made by the coordinator and never reused while a higher one stands; names are numbers, so there is no free text to validate.
- The approval's placement is judged before the approval is made and made with it; the one gap (the store failing after both were judged)
  leaves the approval and is logged.
- Drag in the edit mode places by the same call as pick-and-click; the HTML5 drag events need no library.
- Packages is not in the menu until its page lands; Release, Rollout and Holds keep their pages and routes until the package page takes them.
