# Neersagar ERP – User Guide

For everyone who works in the plant: store keepers, blow-molding and bottling operators, Lab QC, loaders, gatekeepers, drivers, jar-yard staff, managers and the administrator.

This guide explains **what each screen is for, the daily steps, and what to do when something looks wrong.** You do not need to be technical. If a message on screen is unclear, find it in [section 12](#12-messages-you-may-see-and-what-to-do).

> **One rule above all:** nothing is ever deleted. If you made a mistake, you *cancel* or *void* the document with a reason, and the system puts the stock back. Every stock movement is written to a permanent ledger with who did it and when.

---

## Contents
1. [Getting started](#1-getting-started)
2. [Finding your way around](#2-finding-your-way-around)
3. [Dashboard](#3-dashboard)
4. [Raw materials & purchasing](#4-raw-materials--purchasing-store-keeper)
5. [Production](#5-production-operators)
6. [Lab QC](#6-lab-qc)
7. [Warehouse & godowns](#7-warehouse--godowns)
8. [Loading, gate pass & returns](#8-loading-gate-pass--returns-loaders-gatekeepers-drivers)
9. [Returnable jars](#9-returnable-jars-20-l-containers)
10. [Maintenance & vehicle papers](#10-maintenance--vehicle-papers)
11. [Your profile, notifications & the phone app](#11-your-profile-notifications--the-phone-app)
12. [Messages you may see, and what to do](#12-messages-you-may-see-and-what-to-do)
13. [If the internet or server stops](#13-if-the-internet-or-server-stops)
14. [Administrator guide](#14-administrator-guide)
15. [Words used in this system](#15-words-used-in-this-system)
16. [Quick checklists](#16-quick-checklists)

---

## 1. Getting started

### 1.1 Open the app
- **On a phone (Android/Chrome):** open the address your administrator gave you, sign in, then tap the browser menu → **Add to Home screen**. It now opens like a normal app.
- **On a computer:** just open the address in Chrome, Edge or Firefox.
- If your plant uses its own Wi-Fi network, your administrator may ask you to install a small **certificate** on the phone once. Without it the browser shows a security warning.

### 1.2 First time only – creating the administrator
The very first person to open a brand-new system sees a **Setup** screen.
1. The administrator runs the server and finds a line in its log: **"FIRST-RUN SETUP TOKEN: …"**.
2. On the Setup screen enter that token, a username, full name and a strong password (10 or more characters).
3. After the first administrator exists, the Setup screen locks itself permanently.

### 1.3 Signing in
- Enter your **username and password**. Sign in again whenever the screen asks (for safety, unused sessions expire).
- After several wrong passwords the account is **locked for a few minutes**. Wait, or ask the administrator to unlock it.
- **Sign out** from your profile page, especially on shared phones.

### 1.4 Which screens will I see?
The administrator decides which screens each person can open. You will only see the ones you need. If you try to open a screen you do not have, the app takes you back to your first allowed screen with a short message.

| Typical role | Screens |
|---|---|
| Store keeper | Raw materials, Warehouse |
| Operator | Production, Dashboard |
| Lab QC | QC, Dashboard |
| Loader | Loading, Warehouse (view), Dashboard |
| Gatekeeper | Loading, Dashboard |
| Jar yard | Jars, Maintenance, Dashboard |
| Manager | Everything except Admin |
| Administrator | Everything |

---

## 2. Finding your way around

- **Phone:** a bar at the bottom shows your main screens; **More** opens the rest. The top bar shows the page title, a **live-connection light**, the **bell** (alerts) and, if you work in more than one godown, the **godown chooser**.
- **Computer:** the same items appear as a menu on the left.
- **Godown chooser:** pick which godown (storage location) you are working in. Everything you enter then applies to that godown. If you only have one godown, the chooser is hidden. Some read-only screens also offer **All godowns** (totals).
- **Live updates:** screens refresh by themselves when someone else changes something (for example QC approves a batch). You do not need to reload. The light in the top bar shows whether live updates are connected.
- **Tables on phones** turn into **cards**: the item name and status on top, the key numbers below, buttons at the bottom.
- **Colours always come with words and icons** – green *APPROVED / OK*, amber *QUARANTINE / LOW / DUE SOON*, red *REJECTED / OVERDUE / OUT*.

### Crates and pallets
Finished goods are counted in **crates**. Each product has a standard pack: *bottles per crate* and *crates per pallet*. Quantity fields show a small echo such as **"= 2 pallets + 5 loose crates"** so you can catch typing mistakes. Always check that echo before you confirm.

---

## 3. Dashboard

The first screen for most people. It shows only what your role can open.

- **Tiles:** waiting for QC, low stock, open reorders, finished stock (crates), trucks on road, godown transfers in transit, jars overdue, maintenance due, and whether the **hourly water test** is overdue. Tap a tile to jump to that screen.
- **Lists:** batches waiting for QC, low raw materials, trips in progress, transfers taking too long (over 48 hours), recent alerts.
- **By godown:** if you have several godowns, stock per godown.

---

## 4. Raw materials & purchasing (store keeper)

Screen: **Raw materials**. Tabs: **Stock · Reorder · Purchase orders · Receive (GRN) · Returns**.

Raw materials are preforms, caps, labels, sleeves, shrink film, cartons, minerals, chemicals and the empty bottles made on the blow-molding machine. Quantities are kept in the **stock unit** (pieces or kg). Purchases are made in the **purchase unit** (a bag, a roll, a carton). The system converts for you, for example *1 bag = 2000 pieces*.

### 4.1 Check stock
**Stock** tab: each material shows what is in the store, a status (**OK / LOW / OUT**), days of cover, and the average daily use. Tap an item to see its **lots** (each delivery), their **expiry dates**, and recent movements. Oldest-expiry lots are used first (**FEFO**).

### 4.2 Reorder suggestions
**Reorder** tab: the system watches stock every few minutes. It suggests ordering when a material is **below its minimum** or when the **daily use rate** means it will run out before a new delivery can arrive (supplier lead time + safety days). It shows a suggested quantity in purchase units and the supplier.
- **Create PO** turns the suggestion into a purchase order.
- **Dismiss** hides it for a day.
- **Run now** forces a fresh check.

### 4.3 Purchase order (PO)
**Purchase orders** tab → **New**: choose the supplier, add lines (material + quantity in purchase units). The screen echoes the converted quantity ("= 24 000 pcs"). Save. You can **cancel** a PO only while nothing has been received against it.

### 4.4 Receiving goods (GRN – goods receipt note)
**Receive (GRN)** tab:
1. Choose the **godown** you are receiving into, the **supplier** and, if there is one, the **PO**.
2. Enter the supplier's **challan number** and the vehicle number.
3. Add a line per material: quantity in purchase units, **lot number** and **expiry date** if printed on the pack.
4. Tap **Review**. A sheet shows exactly what will be added to stock. Check it, then **Confirm**.
5. Print the receipt if you need a copy.

Safety nets:
- The same **supplier + challan number cannot be entered twice.**
- Receiving far more than ordered (over 110 %) is blocked.
- An unusually large quantity asks "are you sure?".
- **Entered a GRN by mistake?** Open it and **Void** it with a reason. This works only while all of that stock is still in the store (if it was already used, use a Supplier return or an adjustment instead).

### 4.5 Supplier returns
**Returns** tab: send faulty goods back. Choose supplier, (optionally) the GRN, the lot and quantity, and a reason. Stock leaves the store, and if tied to a GRN the PO line is reopened so the replacement delivery can be received.

---

## 5. Production (operators)

Screen: **Production**. Tabs: **Water · Blow · Batches**.

### 5.1 Hourly water test (Water tab)
Record **TDS, pH, ozone, UV status** and any **mineral dosing** at least once an hour. The screen shows when the last test was done and turns red when the hour is overdue. Values outside the limits (set by the administrator) are flagged with an icon and words, and an alert goes out.

### 5.2 Blow molding (Blow tab)
Blow molding turns **preforms** (the small test-tube-shaped PET pieces) into empty bottles.
1. Pick the product – the right preform and bottle are filled in for you.
2. The screen shows **preforms available** in the store before you start. You cannot issue more than exist.
3. Enter **preforms issued** and **good bottles made**. Scrap is calculated for you (in pieces, % and kilograms).
4. If scrap is above the plant's limit, a **note is required**.

Good bottles go to the blow floor; scrap goes to the scrap yard.

### 5.3 Making a batch (Batches tab)
A batch is one filling run of one product.
1. Choose the **product**, **shift** and the number of **crates** (the pallet echo helps).
2. The **BOM check** panel lists every material the batch will use – empty bottles, caps, labels, sleeves, film, cartons, and **minerals dosed per 1000 litres** – with *required vs available*. Anything short is shown in red **before** you submit.
3. Optional: add **waste** (broken caps, rejected labels …) with a reason, so stock stays true to what really happened.
4. Quality gate: if there was **no water test in the last hour** you see a warning; if the last water test was **out of range** you must confirm explicitly.
5. **Review** → **Confirm**. All materials are deducted at once, and the finished crates enter **QC quarantine**.

A new batch **cannot be sold or loaded until Lab QC approves it.** Tap a batch for details: materials used, tests, and a **trace** view (where every crate went – useful for a recall).

> **Returnable (20 L) products** do not use preforms or bottles. They use clean containers from the jar yard plus caps/seals, labels and minerals.

---

## 6. Lab QC

Screen: **QC**. Shows the **quarantine queue** with how long each batch has been waiting.

1. Open a batch.
2. **Add a test result**: TDS, pH, ozone, turbidity, microbial result, seal and label checks, **PASS or FAIL** and remarks.
3. **Approve** – allowed only when the **latest test is PASS**. The stock moves to *finished goods ready* and loaders can use it.
4. **Reject** – a reason is required. The stock moves to the scrap yard.

Decisions are **final** and cannot be reversed. If someone else decides the same batch while you have it open, a banner tells you and the buttons disappear.

---

## 7. Warehouse & godowns

Screen: **Warehouse**. Tabs: **Overview · Stock / Expiring · Ledger · Actions**. Use the godown chooser at the top.

### 7.1 Zones
Stock is kept by **zone** inside each godown:

| Zone | Meaning |
|---|---|
| RM store | raw materials |
| Blow floor | empty bottles |
| QC quarantine | finished goods waiting for QC |
| FG ready | approved finished goods, can be loaded |
| Scrap yard | rejected, damaged or written-off goods |
| Jar empty / Jar clean | returnable containers waiting to be washed / ready to fill |

### 7.2 Looking at stock
- **Overview:** totals per zone, per godown, and goods in transit.
- **Stock / Expiring:** every item, zone and lot in oldest-expiry-first order. The **Expiring** list shows lots close to or past expiry. **Export CSV** downloads the list for a spreadsheet.
- **Ledger:** every movement ever made – filter by item, zone, type or date. It cannot be edited.

### 7.3 Actions (everything asks for a reason and shows a review first)
- **Adjustment:** correct stock after a count or a found/lost item. Choose a reason code and write a short reason. Large changes (over 20 % of the balance by default) ask you to confirm. You cannot raise finished goods above what the batch produced.
- **Move within the godown:** for example raw material from store to blow floor, or any item to the scrap yard (write-off).
- **Write-off / destroyed stock:** choose a **reason** (breakage, expired, contaminated, spillage, QC reject, theft/loss, transit damage, other), add a note, and confirm. A printable **voucher** is produced.
- **Physical stock count:** create a count sheet for a zone, enter what you actually counted (phone-friendly list), review the **variances**, then **Post** to correct the books.

### 7.4 What the system will not let you do
- Move stock **into or out of QC quarantine** by hand – only QC can release it.
- Put finished goods into **FG ready** without QC approval.
- Take stock that is **reserved by an open loading trip** (message *RESERVED STOCK* lists the trip numbers – remove those crates from the trip first).
- Let any balance go **below zero**.

### 7.5 Godown transfers (moving stock between locations)
Screen: **Godown transfers** (under Warehouse).

**Sending:**
1. **New transfer**: choose *from* and *to* godowns, the vehicle and driver, and add lines (item, lot, zone, quantity). Only approved finished goods and normal raw materials can be moved.
2. **Review → Confirm.** The stock leaves your godown and is shown **in transit**. Print the transfer note for the driver.

**Receiving (at the other godown):**
1. Open the transfer → **Receive**.
2. For every line enter **received** and **damaged**. The screen calculates the **shortage**. A shortage needs a note and a confirmation.
3. Received goods are added to your godown; damaged goods go to your scrap yard.

**Cancel** a transfer (with a reason) while it is still in transit; the stock returns to the sending godown. Transfers in transit for more than 48 hours are flagged on the dashboard.

---

## 8. Loading, gate pass & returns (loaders, gatekeepers, drivers)

Screen: **Loading**. Main steps: **Trips → New trip → Load → Gate pass → Settle**. Also **Direct sales** (walk-in dispatch).

### 8.1 Start a trip
**New trip**: choose the **vehicle**, route and driver. The trip loads from the godown selected in the top bar and **cannot change godown later**. A vehicle can have only one open loading trip at a time.

### 8.2 Load
The load list shows what is available – by product and batch, oldest expiry first – as **free = stock minus what other trips have reserved**.
- Use the **pallets / crates** stepper and tap **Add**. The capacity bar shows how full the vehicle is and warns at 95 %.
- Only **QC-approved** stock can be loaded. A batch still waiting for QC shows a lock.
- If two loaders try to take the last crates, only one succeeds; the other sees the new availability immediately.
- Remove a line any time before the gate pass.

> If your plant uses **QR stickers** (an optional feature the administrator can switch on), you can also add a set by **scanning its QR code** with the phone camera, or typing the code. If you load only part of a set, the app creates a new sticker for the remainder – print it.

### 8.3 Gate pass
When loading is complete tap **Gate pass**.
1. The **review sheet** shows exactly what will leave stock and checks the vehicle's papers (insurance, fitness, PUC, permit). An expired paper **blocks** the gate pass.
2. Confirm. The stock is deducted, the trip becomes **Dispatched**, and a **gate pass number** (GP-…) is issued.
3. **Print** the gate pass (A4 or 80 mm slip) for the driver and the gate.

If you tap twice, the second tap does nothing – the gate pass is issued only once.

### 8.4 Settling the trip (when the vehicle returns)
Open the dispatched trip → **Settle**. For every item enter **delivered**, **returned** (unsold, still good) and **damaged**. The three numbers must add up to what was loaded; the screen shows the check live.
- **Returned** crates go back into stock; **damaged** crates go to the scrap yard. A high damage share asks for confirmation.
- For **returnable jars**, add a line per customer: jars delivered, empties collected, damaged and lost. The customer lines must add up to the jars delivered.
- Review → Confirm. A trip can be settled **once**.

### 8.5 Direct sales (dispatch notes)
For a walk-in buyer who collects goods without a truck trip: **Direct sales → New**, choose the customer (or type a party name), add products and crates (oldest stock is picked automatically), review and confirm. Print the dispatch note. Quantities only – the system does not handle prices, tax or payments.

### 8.6 Cancelling and voiding
- **Cancel a trip** that is still loading: crates are released.
- **Void a gate pass** (wrong goods, vehicle never left): allowed within the time window set by the administrator (the administrator can always do it), needs a reason and a confirmation. All stock is put back.
- **Void a dispatch note**: same rules.

---

## 9. Returnable jars (20 L containers)

Screen: **Jars**. Tabs: **Overview · Customers · Plant actions · Destroyed**.

The jars are reused again and again. The system tracks the whole **fleet**: how many are at the plant (empty, clean, filled), with customers, on trucks, and how many were **destroyed** and why.

- **Overview:** fleet size, destroy rate, **average fills per jar versus the retirement age**, and an alert when either is high. Pick a period; with several godowns you also see a breakdown per godown.
- **Customers:** who holds how many jars, how long the oldest ones have been out, and **overdue** customers. Tap a customer for their ledger. You can enter an **opening balance** when you start using the system.
- **Plant actions** (each shows a review first):
  - **Receive new containers** – newly bought jars join the fleet.
  - **Wash** – jars washed from the empty pile to the clean pile; enter how many were **rejected** in washing.
  - **Receive empties** – a customer returns jars at the counter (good and damaged counted separately).
  - **Counter issue** – hand filled jars to a customer.
  - **Destroy** – retire damaged or lost jars with a **reason** (cracked, leaking, wash reject, damaged return, age retired, lost, other).
- **Destroyed:** the log with a breakdown by reason.

Filling jars is a normal **batch** of the 20 L product on the Production screen; it uses clean jars.

---

## 10. Maintenance & vehicle papers

Screen: **Maintenance**.

- **Schedules:** machine and vehicle service tasks grouped as **Overdue / Due soon / OK** with days left. Tap **Complete** after the job: enter the date and notes, and the next due date is set automatically (or enter it if the task has no fixed interval). Each completion is kept in the task's log.
- **Vehicle documents:** insurance, fitness, PUC and permit expiry per vehicle, colour-coded. An expired document blocks the gate pass for that vehicle.
- Alerts are sent ahead of time (default 30 days before expiry).

---

## 11. Your profile, notifications & the phone app

Screen: **Profile** (tap your name).

- **Change password** – this signs out your other devices.
- **Notifications:** tick the alert types you want (for example *low stock*, *QC approved*, *gate pass issued*, *jars overdue*, *maintenance due*). You only see the types that match the screens you can open.
- **Push notifications on your phone:** tap **Enable push**, allow notifications when the phone asks, and use **Send test** to check. If your administrator has not set up push keys, this part will say it is not available.
- The **bell** in the top bar lists your recent alerts; tap one to open the related screen.
- **Install the app:** browser menu → *Add to Home screen*.

---

## 12. Messages you may see, and what to do

| Message (short) | What it means | What to do |
|---|---|---|
| **Insufficient stock** | Not enough in that zone/godown. The message shows how many are available. | Check the real count. Receive the stock (GRN) or post a counted adjustment first. |
| **Reserved stock** | Some of those crates are held by an open loading trip. The trip numbers are listed. | Open that trip and remove the crates, then retry. |
| **QC locked** | The batch is not approved. | Wait for Lab QC. |
| **Capacity exceeded** | The vehicle would be overloaded. | Reduce the load or use a bigger vehicle. |
| **Vehicle documents expired** | Insurance/fitness/PUC/permit has expired. | Renew it in Maintenance. |
| **This looks unusual – confirm?** | The entry is much larger/smaller than normal, or risky. The reasons are listed. | Re-check the numbers. Tap *Yes, continue* only if they are right. |
| **Duplicate** | This was already entered (for example the same challan). | Open the existing record. |
| **Over produced** | That batch cannot hold more crates than were made. | Check the batch number and quantity. |
| **Already processed / changed by someone else** | Another person or an earlier tap already did this. | The screen refreshes itself; review it and continue if needed. |
| **Same action still being processed** | You tapped again while the first request was working. | Wait a few seconds. Do not tap repeatedly. |
| **Too many attempts / account locked** | Too many wrong passwords. | Wait a few minutes or ask the administrator. |
| **Session ended** | You were signed out (time passed, or the administrator changed your access). | Sign in again – anything you typed is kept as a draft. |
| **Server busy / not saved** | The database was unreachable for a moment. | Wait and tap **Retry**. Nothing was saved. |
| *Ref: xxxxxxxx* | A reference code for a technical problem. | Tell the administrator this code. |

---

## 13. If the internet or server stops

The app is built for weak plant Wi-Fi.

- A red **banner** says *"Server unreachable – nothing is lost, your entries are kept"*. The top-bar light changes.
- What you typed in a long form is **saved as a draft on the phone** and comes back if you reload or sign in again.
- If you tap Submit and the network drops, the button shows **"Not sent – Retry"**. Tap **Retry** when the connection is back. Retrying is **safe**: the system recognises the same request and **never creates a duplicate** (even if the first attempt had actually gone through).
- Stock-changing actions are **not queued silently** while offline – you always see whether something was saved.
- While the server restarts after an update, the banner says *"Server restarting…"* and the app reconnects by itself.

---

## 14. Administrator guide

Screen: **Admin**. Tabs: **Users · Sessions · Audit · Settings · Master data · Godowns**.

### 14.1 Users
- **Create a user:** username, full name, a temporary strong password, and tick the **screens** they may open. Optionally limit them to certain **godowns** and choose which **notifications** they get by default.
- **Edit / deactivate / unlock / reset password.** Deactivating a user ends their sessions. The system never lets you remove the **last administrator**.
- **Sessions:** see where people are signed in; **revoke** one session or all of them.

### 14.2 Godowns
Add each storage location (code, name, address, contact). The **Main Plant** exists from the start and cannot be removed. A godown that still holds stock or has an open trip/transfer cannot be deactivated. Mark a location as a **production site** if batches and QC happen there.

### 14.3 Master data
Create and maintain:
- **Suppliers** (lead time in days), **raw materials** (category, stock/purchase unit and conversion, preform weight, minimum stock, default supplier),
- **Products** – *Bottled* (single-use) or *Returnable* (reusable jar); bottles per crate; **crates per pallet**; shelf life; for bottled products the **preform** and **empty bottle** they use; for returnable ones the **maximum fills** before retirement,
- **Recipes (BOM):** the material list per crate. Minerals and chemicals are normally set **per 1000 litres of water** so the same recipe works for every bottle size,
- **Customers, routes, vehicles** (capacity and document expiry dates), **equipment** and **maintenance schedules**.

Items are *deactivated*, never deleted, so history stays intact.

### 14.4 Settings (changes apply immediately, no restart)
Plant name and code, session length, lockout rules, **burn-rate window and safety days** for reorder suggestions, how often the stock checker runs, jar overdue days, alert lead times, **QC strict lock**, **QR labels on/off**, water-test limits (pH/TDS), **plant time-zone offset**, and the confirmation thresholds (large adjustments, scrap %, damage %, void window, jar destroy-rate alert).

### 14.5 Audit
Every important action (logins, setting changes, QC decisions, gate passes, voids, adjustments …) is recorded with who, when and details. Filter by user, action, entity or date. It is read-only.

### 14.6 Keeping the system safe
- Give each person **only** the screens they need.
- **Deactivate** leavers immediately.
- Use strong passwords; nobody should share an account.
- Keep **backups** running (see the developer guide: nightly `pg_dump` timer) and test a restore now and then.
- Push notifications need push keys on the server (`water-erp gen-vapid`).

---

## 15. Words used in this system

| Word | Meaning |
|---|---|
| **Godown** | A storage location (warehouse). |
| **Zone** | A section inside a godown (RM store, FG ready, quarantine …). |
| **Lot / batch** | One delivery of a raw material (lot) or one production run of a product (batch). |
| **FEFO** | First-expiry, first-out: the oldest-expiry stock is used first. |
| **Preform** | The small test-tube-shaped PET piece that is blown into a bottle. |
| **Blow molding** | The machine step that turns preforms into bottles. |
| **BOM / recipe** | The list of materials needed per crate. |
| **Quarantine** | Finished goods waiting for Lab QC; cannot be loaded. |
| **GRN** | Goods Receipt Note – recording that goods arrived. |
| **PO** | Purchase order. |
| **Gate pass** | The permit to let a loaded vehicle leave. |
| **Settlement** | Closing a trip by recording delivered / returned / damaged. |
| **Reserved** | Crates added to an open trip; held back but not yet gone. |
| **Void** | Cancelling a document with a reason; stock is put back. |
| **Idempotent / safe retry** | Sending the same request again never creates a duplicate. |
| **Ledger** | The permanent list of every stock movement. |

---

## 16. Quick checklists

**Every hour (operator):** record the water test.

**Start of shift (operator):** check preform and material availability → blow molding → create the batch (check the BOM panel) → tell QC.

**QC:** open the queue → test → PASS → Approve (or FAIL → Reject with a reason).

**Receiving a delivery (store):** match the challan to the PO → GRN → review → confirm → print.

**Loading a vehicle (loader):** new trip → add crates (watch the capacity bar and QC lock) → check the vehicle papers → gate pass → print.

**Vehicle returns (gate):** open the trip → settle: delivered + returned + damaged = loaded → jars per customer → confirm.

**End of day (store/jar yard):** write off breakages with a reason → count one zone → post the count → wash the day's empties.

**Something went wrong?** Do not delete anything: use **Void / Cancel / Return / Adjustment** with a reason. Note the *Ref* code if you see one and tell the administrator.
