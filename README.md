# Wayfarer trip planner

A small Rust + SQLite trip planner for organizing a group itinerary and sharing it with family or friends. The application stores data locally in `data/trips.sqlite` and photos under `data/photos/` by default. It serves the interface at `http://127.0.0.1:3007` by default.

## Run locally

You need a Rust toolchain with Cargo. Run:

```sh
./run.sh
```

`run.sh` builds and starts the planner in the background. It checks the configured TCP port first and stops any process already listening there. The PID file and log are stored at `data/xjaroo-trip.pid` and `data/xjaroo-trip.log`.

Manage the background app with:

```sh
./run.sh status
./run.sh restart
./run.sh stop
./run.sh migrate
```

`./run.sh migrate` applies the SQLite schema and legacy PIN migration without starting the web server. It refuses to run while a process is listening on the configured trip planner port and makes a timestamped copy of an existing database first. Trips with no PIN receive `2850`; trips that already have a PIN are left unchanged. Run it on the server with `.env` pointing `TRIP_DB` at the existing database, then start the updated application.

Open the printed address and create a trip with its own four-digit PIN. Add stops with date ranges, then add optional daily activities inside each stop. The trip form records the travelers, their home base, and a contact back home. The dashboard highlights the stop covering today's date. Anyone with the trip link and PIN can view and edit the plan. Use **Copy trip** to start a fresh plan from stops and costs. Photos are resized in the browser to a maximum 1400 px edge and JPEG compressed before they are saved locally. **Save HTML** downloads a snapshot and **Save PDF** opens the browser print dialog.

## Sharing and privacy

No account or email address is required. The home page lists every trip, including old, private, and public trips. Trip names and dates are visible in that list; opening any trip requires its four-digit PIN. Anyone with the PIN can see and edit the complete trip, including contacts, costs, comments, and photos. Treat the PIN as an edit credential and share it only with people you trust.

On startup, trips in the existing database without a PIN are migrated to a salted Argon2 hash of `2850`. This applies once; changing a trip PIN is preserved across restarts. New trips require a PIN chosen by the creator and do not use `2850` automatically. Enter the PIN in **Edit trip** to replace it.

PIN attempts are limited to five failures per client IP and browser user-agent in a rolling 15-minute window. On Cloudflare, the client IP is read from `CF-Connecting-IP`; only the IP and user-agent rate-limit keys are held in server memory, not saved with the trip, and the attempt counters reset when the app restarts. A four-digit PIN is a convenience gate, not strong authentication. The **Change history** panel stores the state before each trip edit and can restore that version. Restoring also creates a history entry. History is stored in the local SQLite database.

Copy `.env.example` to `.env` to configure the app. Set `TRIP_ADDR=0.0.0.0:3007` to listen on all network interfaces, `TRIP_PUBLIC_URL=https://trips.example.com` to use your public host in copied trip links, and `TRIP_DB=/path/to/trips.sqlite` to choose a database location. `TRIP_ADDR` defaults to `127.0.0.1:3007`; `TRIP_PUBLIC_URL` defaults to the address used in the browser. Photos are stored in a sibling `photos` directory beside the database. For an internet-facing deployment, put the app behind HTTPS and add backups and a recovery process for lost trip PINs before sharing broadly. The trip link shows the trip name and dates; the four-digit PIN grants full view and edit access.

Place search uses the public OpenStreetMap Nominatim service by default. Searches are only sent after the user presses **Search**; the app does not autocomplete while typing. Results request an English address plus the original/local place name when OpenStreetMap has one. Results are cached and the server spaces uncached requests at least one second apart. Search terms are sent to OpenStreetMap, so avoid private or confidential text. `GEOCODER_URL` can point to another Nominatim-compatible endpoint. The public endpoint is intended for modest usage; choose a hosted or self-run geocoder if the app grows. Google Places is another option, but it requires a Google Cloud API key and billing to be enabled ([Google setup](https://developers.google.com/maps/documentation/places/web-service/get-api-key)).

The journey map uses MapLibre GL with OpenFreeMap vector tiles and English-first labels. It uses an English name where available, then a Latin-script name, then the local name. It numbers mapped stops in date order, draws a line between them, and fits the view to all mapped stops. Set `MAP_STYLE_URL` to another compatible MapLibre style URL. Map attribution is shown on the map; OpenFreeMap attribution terms also allow printed exports ([OpenFreeMap quick start and attribution](https://openfreemap.org/quick_start/)).

## Current scope

- Multiple trips with editable dates, traveler count, and currency.
- Multi-day stops and stays with optional dated activities nested inside each stop.
- Home base, travelers' names, return date, and a home emergency contact.
- A current-location banner based on today's date and the stop ranges.
- Map panel with OpenStreetMap pins when latitude and longitude are added to a stop.
- Search places by name through a server-side Nominatim proxy; results are cached and requests are rate-limited. Selecting a result saves its name and coordinates on the stop.
- Shared comments/questions and trip-level expense tracking, including per-person estimate.
- Copy a trip and print/export the current plan through the browser.
- Compressed local photo album shared with the trip.
- PIN-protected trip links with a complete change history and rollback.

## Next useful increments

1. Add multiple emergency contacts and traveler invitation/recovery flows.
2. Add map route lines between stops and improve multi-provider geocoding options.
3. Add optional comment moderation and photo deletion.
4. Add expense splits, receipts, and planned-versus-actual budgets.
5. Add database and photo backup/restore for a small server deployment.
