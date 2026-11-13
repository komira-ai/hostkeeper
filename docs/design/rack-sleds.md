# Sleds: several servers on one tray of a rack

Status: design, 2026-11-13, for komira-ai/hostkeeper. Stacked on the network-gear PRs (#373 API, #374 UI). The repository copy of this
file is `docs/design/rack-sleds.md` (same text).

Owner direction, in order, the last wins: "we also need to support the idea of multiple servers the same shelf in a rack. Like 2 mac
studios fit in a 3u shelf, and 3 nucs fit into a 2u shelf"; "cluster is our lab and then row and rack are kind of fixed. Sleds go into the
rack at certain u positions and height. And then sleds can have multiple slots"; "we could say C0-R0-R0-S0-S0 is spoolr-node-06 at the
moment"; and the UX: "it makes more sense to place sleds into the rack and then assign a server to a sled. Like it should have been add
sled and select sled size and sled slots and u location, and then make it draggable to move things around."

## 1. Problem

Today a placement is (rack, lowest U, height U) for one server, so a U holds one thing. Two Mac Studios side by side on a 3 U tray, or
three NUCs on a 2 U tray, share the same U. And the way to place a server (pick a size, then the row, rack and U) asks for the
hardware's shape every time a server is racked, when the shape belongs to the tray it sits in.

## 2. Model

Cluster is the lab (one, always C0). Rows and racks are fixtures added rarely. A **sled** is what goes in a rack:

```
Sled { id sled-<n>, rack, u (lowest U, 1 is the bottom), height_u 1..60, slots 1..8, label (optional, 0..32 chars) }
```

- Sleds are the only things that occupy U. The U-collision check is between sleds of one rack.
- An **item** (an enrolled host, an inventory server, a switch) stands on one slot of one sled: `(sled, slot)`, slot from 0, counted from
  the left as one faces the rack. An item has no U or height of its own; a 4 U server is on a 4 U one-slot sled, a 1 U switch on a 1 U
  one-slot sled, two Mac Studios on a 3 U two-slot sled, three NUCs on a 2 U three-slot sled.
- A slot holds one item; an item is on at most one slot. A sled is made first (empty) and items are assigned to it afterwards.
- Address: every item has **C\<cluster\>-R\<row\>-R\<rack\>-S\<sled\>-S\<slot\>**, all zero-based: `C0-R0-R0-S0-S0`. The sled number is its
  place in its rack counting from the bottom (the lowest sled is S0), so it changes when a sled is put below it. Computed by the
  coordinator on every read, never stored; stored are numbers (rack, U, slot). The row and rack in the address are the layout's numbers
  less one while rows and racks are numbered from 1 (see 7, question 4).

### Rules (judged whole by `Layout::build`, as every layout rule is)

- a sled is in a rack the layout holds, within U 1 to the rack's height, 1 to 60 U high, 1 to 8 slots; at most 4096 sleds;
- sleds of one rack share no U (`overlap`), an item's slot is within its sled's slots and holds one item (`overlap`);
- moving a sled (new rack, new U) moves everything on it, since items name the sled; a move onto occupied U is `overlap`, past the top
  `out_of_rack`;
- a sled made smaller than the slot of an item on it, or removed with items on it, is `in_use` naming the items; a rack that holds a sled
  is `in_use` too; a device removed or an item unplaced frees its slot;
- a placement that names no slot takes the lowest free slot; a full sled is `capacity`.

## 3. API (`layout.proto`, additions only)

- `message LayoutSled { id, rack, number (computed), u, height_u, label, address (computed), slots = 8 }`; `GetLayoutResponse.sleds = 6`.
- `rpc PutSled` (empty `id` adds an empty sled; with an `id` it replaces rack, U, height, slots and label: that is the move, the resize
  and the drag), `rpc RemoveSled`. `PutLayoutSledRequest.slots`: 0 on an add is 1, 0 on a replace keeps the count.
- `LayoutPlacement` gains `sled = 6`, `slot = 7`, `address = 8`; for a client of the release before `rack`, `u` and `height_u` stay
  filled with the sled's. `PlaceLayoutRequest` gains `sled = 7`, `slot = 8` (optional: the lowest free). Assigning, moving an item between
  slots or sleds, and unassigning are `Place` and `Unplace`: one call each. `Place` with `rack`, `u`, `height_u` (the old form) still
  works and makes a one-slot sled that goes with its item (`implicit`); no page uses it any more.
- `LayoutRefusal.UNKNOWN_SLED = 15`; `in_use`, `overlap`, `out_of_rack`, `invalid_request`, `capacity` carry the rest. Audit
  actions `layout_sled_put`, `layout_sled_remove`.
- Enrollment: `ApproveEnrollmentRequest` gains `placement_sled = 12`, `placement_slot = 13` (a sled slot; the old rack, U, height form stays).

## 4. Storage and migration (B8)

- File `<state-file>.layout` gains `sleds` and `sled_items`; `placements` is read (turned into one-slot implicit sleds, in the order of
  rack, U and key, ids from 1) and no longer written. A sled record without `slots` is a one-slot sled.
- Migration `0017_lab_sleds.sql` (`may-run`) adds `lab_sleds`, `lab_sled_items` and turns the rows of `lab_placements` into one-slot
  sleds, once. `lab_placements` is left as it was and not read again.
- Old release on new data: it reads `GetLayoutResponse` and `LayoutPlacement` with the sled fields skipped, sees every item placed on
  its sled's U (the repeated `rack`, `u`, `height_u`), and an empty sled not at all; on Postgres it never reads the new tables and keeps the
  placements as they were at 0017 (a rollback shows the lab as it was before sleds). On the file store the release before ignores the
  `sleds` keys, finds no `placements`, and so sees an unplaced lab; the operator re-places after a rollback (accepted, question 3).
- New release on old data: the old file's and tables' placements become sleds on first start.

## 5. UI

- **Edit layout, rack view: Add sled** asks Size (height in U), Slots (1..8, default 1), U location (only the U where that size fits). The
  new sled draws as an empty tray with N empty bays. A sled has Resize/Move/Remove (Remove names what stands on it when refused).
- **Assigning**: click an empty bay, pick from the unplaced items (servers and switches); Unassign on an item. Enrollment approval offers
  Host group and an optional "Sled slot" (the free slots by address, e.g. C0-R0-R0-S2-S0); no height or U.
- **Drag and drop in Edit layout**: drag a sled up or down its rack or to another rack (everything on it moves; only where it fits, the
  target outline shows, a refusal otherwise); drag an item between slots or from the Unplaced tray onto an empty slot. Keyboard: select a
  sled or item, arrow keys move it, Enter drops; a Move... dialog does the same. Each move is one `PutSled` or `Place` call; the screen
  redraws from the reply of `Get`, so the API is the source of truth.
- **Row view**: a sled is drawn across its U range; a one-slot sled with an item is exactly today's server or switch faceplate (no visible
  tray); a sled of more than one slot is a tray with N bays, each a small Mac Studio / NUC style box for its item (empty: a gap), clickable to the host
  page, same state colours.
- **Address** is shown on the host page, the switch page, Inventory rows and the row view tooltip, and is a search on Home's List view.

## 6. Tests (named in the PR)

Coordinator: sled rules (collision, bounds, slot bounds, slot count, shrink under an item), move-with-sled, remove refused naming items,
rack removal refused, the address, the placement forms, file and Postgres round trip, the old placements becoming sleds (file and
migration). Proto (B8, both directions). UI through the test kit: add sled (what it sends), assign by clicking a bay, drag of a sled and of an item
(what each sends, the refusal shown, the keyboard form), the approval's single Sled slot choice, the address on pages.

## 7. Open questions and defaults

1. Limits: 1 to 8 slots, 1 to 60 U, 4096 sleds. Default as stated.
2. Slot order left to right from the front. Default yes.
3. Rollback on the file store loses placements (older release ignores the new keys). Default: accept, document.
4. Zero-based labels: branch `zero-based-rows` makes new rows and racks start at 0. Until it lands the address uses number less one;
   after it lands the number is used as is. Default: merge main in when it lands and drop the subtraction in the same commit.
5. Mixed gear on a sled (a Mac Studio beside a NUC): allowed; the sled does not know what it carries.
6. Faceplate look by name ("studio", "nuc"): cosmetic, never a record field.
