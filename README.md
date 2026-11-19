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
```

Open the printed address and create a trip. Add stops with date ranges, then add optional daily activities inside each stop. The trip form records the travelers, their home base, and a contact back home. The dashboard highlights the stop covering today's date. **Family link** is for viewing and comments. **Traveler edit link** also allows changing the plan, costs, and photos; share it only with travelers. Both links use unguessable UUIDs. Use **Copy trip** to start a fresh plan from stops and costs. Photos are resized in the browser to a maximum 1400 px edge and JPEG compressed before they are saved locally. **Save HTML** downloads a snapshot and **Save PDF** opens the browser print dialog.

Copy `.env.example` to `.env` to configure the app. Set `TRIP_ADDR=0.0.0.0:3007` to listen on all network interfaces, `TRIP_PUBLIC_URL=https://trips.example.com` to use your public host in copied family and traveler links, and `TRIP_DB=/path/to/trips.sqlite` to choose a database location. `TRIP_ADDR` defaults to `127.0.0.1:3007`; `TRIP_PUBLIC_URL` defaults to the address used in the browser. Photos are stored in a sibling `photos` directory beside the database. For an internet-facing deployment, put the app behind HTTPS and add backups and a recovery process for lost traveler edit links before sharing broadly. The links are bearer capabilities: anyone who gets a family link can see the trip and contact information; anyone with an edit link can change it.

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
- Separate family view/comment and traveler edit links.

## Next useful increments

1. Add multiple emergency contacts and traveler invitation/recovery flows.
2. Add map route lines between stops and improve multi-provider geocoding options.
3. Add optional comment moderation and photo deletion.
4. Add expense splits, receipts, and planned-versus-actual budgets.
5. Add database and photo backup/restore for a small server deployment.
