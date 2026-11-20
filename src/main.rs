use argon2::{
    password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Response},
    routing::{delete, get, post},
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::Utc;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use tower_http::services::ServeDir;
use uuid::Uuid;

#[derive(Clone)]
struct AppState {
    db_path: Arc<PathBuf>,
    photo_path: Arc<PathBuf>,
    geocoder_last_request: Arc<tokio::sync::Mutex<Option<Instant>>>,
    geocoder_url: Arc<String>,
    geocoder_client: Arc<reqwest::Client>,
    pin_attempts: Arc<tokio::sync::Mutex<PinAttempts>>,
}

#[derive(Default)]
struct PinAttempts {
    failures: HashMap<String, Vec<Instant>>,
}

impl PinAttempts {
    fn blocked(&mut self, keys: &[String], now: Instant) -> bool {
        self.prune(now);
        keys.iter()
            .any(|key| self.failures.get(key).is_some_and(|times| times.len() >= 5))
    }

    fn fail(&mut self, keys: &[String], now: Instant) {
        self.prune(now);
        for key in keys {
            self.failures.entry(key.clone()).or_default().push(now);
        }
    }

    fn clear(&mut self, keys: &[String]) {
        for key in keys {
            self.failures.remove(key);
        }
    }

    fn prune(&mut self, now: Instant) {
        self.failures.retain(|_, times| {
            times.retain(|at| now.duration_since(*at) < Duration::from_secs(15 * 60));
            !times.is_empty()
        });
    }
}

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

type ApiResult<T> = Result<T, ApiError>;

async fn db<T: Send + 'static>(
    state: &AppState,
    f: impl FnOnce(Connection) -> ApiResult<T> + Send + 'static,
) -> ApiResult<T> {
    let path = state.db_path.as_ref().clone();
    tokio::task::spawn_blocking(move || {
        let conn = Connection::open(path).map_err(internal)?;
        conn.execute_batch("PRAGMA foreign_keys = ON;")
            .map_err(internal)?;
        f(conn)
    })
    .await
    .map_err(internal)?
}

fn internal<E: std::fmt::Display>(e: E) -> ApiError {
    ApiError(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}
fn bad(message: &str) -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, message.to_owned())
}
fn not_found() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "Trip not found".into())
}
fn now() -> String {
    Utc::now().to_rfc3339()
}

fn valid_pin(pin: &str) -> bool {
    pin.len() == 4 && pin.bytes().all(|byte| byte.is_ascii_digit())
}

fn hash_pin(pin: &str) -> ApiResult<String> {
    let salt = SaltString::encode_b64(Uuid::new_v4().as_bytes()).map_err(internal)?;
    Argon2::default()
        .hash_password(pin.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(internal)
}

fn verify_pin(pin: &str, encoded_hash: &str) -> bool {
    PasswordHash::new(encoded_hash).ok().is_some_and(|hash| {
        Argon2::default()
            .verify_password(pin.as_bytes(), &hash)
            .is_ok()
    })
}

fn valid_share_mode(mode: &str) -> bool {
    matches!(mode, "private" | "pin" | "public")
}

fn pin_attempt_keys(trip_id: &str, headers: &HeaderMap) -> Vec<String> {
    let ip = headers
        .get("cf-connecting-ip")
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("unknown");
    let user_agent = headers
        .get("user-agent")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("unknown");
    vec![
        format!("trip:{trip_id}:ip:{ip}"),
        format!("trip:{trip_id}:browser:{ip}:{user_agent}"),
    ]
}

async fn require_editor(state: &AppState, id: &str, headers: &HeaderMap) -> ApiResult<()> {
    let supplied = headers
        .get("x-edit-key")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_owned();
    let trip_id = id.to_owned();
    let matches = db(state, move |conn| {
        let key: Option<String> = conn
            .query_row("SELECT edit_key FROM trips WHERE id=?1", [&trip_id], |r| {
                r.get(0)
            })
            .optional()
            .map_err(internal)?;
        match key {
            Some(key) if key == supplied && !key.is_empty() => Ok(()),
            Some(_) => Err(ApiError(
                StatusCode::FORBIDDEN,
                "Traveler edit access is required".into(),
            )),
            None => Err(not_found()),
        }
    })
    .await;
    matches
}

fn init_db(path: &PathBuf) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path)?;
    conn.execute_batch("PRAGMA foreign_keys = ON;
      CREATE TABLE IF NOT EXISTS trips (
        id TEXT PRIMARY KEY, title TEXT NOT NULL, start_date TEXT NOT NULL, end_date TEXT NOT NULL,
        travelers INTEGER NOT NULL DEFAULT 1, currency TEXT NOT NULL DEFAULT 'AUD',
        origin TEXT NOT NULL DEFAULT '', traveler_names TEXT NOT NULL DEFAULT '', emergency_name TEXT NOT NULL DEFAULT '', emergency_phone TEXT NOT NULL DEFAULT '', edit_key TEXT NOT NULL DEFAULT '',
        share_mode TEXT NOT NULL DEFAULT 'private', pin_hash TEXT NOT NULL DEFAULT '',
        created_at TEXT NOT NULL, updated_at TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS itinerary_items (
        id INTEGER PRIMARY KEY AUTOINCREMENT, trip_id TEXT NOT NULL REFERENCES trips(id) ON DELETE CASCADE,
        day TEXT NOT NULL, time TEXT NOT NULL DEFAULT '', title TEXT NOT NULL, place TEXT NOT NULL DEFAULT '',
        category TEXT NOT NULL DEFAULT 'activity', notes TEXT NOT NULL DEFAULT '', cost REAL NOT NULL DEFAULT 0,
        latitude REAL, longitude REAL, end_day TEXT, parent_id INTEGER, kind TEXT NOT NULL DEFAULT 'stop'
      );
      CREATE TABLE IF NOT EXISTS comments (
        id INTEGER PRIMARY KEY AUTOINCREMENT, trip_id TEXT NOT NULL REFERENCES trips(id) ON DELETE CASCADE,
        name TEXT NOT NULL, body TEXT NOT NULL, created_at TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS expenses (
        id INTEGER PRIMARY KEY AUTOINCREMENT, trip_id TEXT NOT NULL REFERENCES trips(id) ON DELETE CASCADE,
        description TEXT NOT NULL, amount REAL NOT NULL, category TEXT NOT NULL DEFAULT 'Other'
      );
      CREATE TABLE IF NOT EXISTS photos (
        id INTEGER PRIMARY KEY AUTOINCREMENT, trip_id TEXT NOT NULL REFERENCES trips(id) ON DELETE CASCADE,
        filename TEXT NOT NULL, caption TEXT NOT NULL DEFAULT '', created_at TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS geocode_cache (
        query TEXT PRIMARY KEY, results TEXT NOT NULL, updated_at TEXT NOT NULL
      );")?;
    ensure_column(&conn, "trips", "origin", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&conn, "trips", "traveler_names", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&conn, "trips", "emergency_name", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        &conn,
        "trips",
        "emergency_phone",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(&conn, "trips", "edit_key", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(
        &conn,
        "trips",
        "share_mode",
        "TEXT NOT NULL DEFAULT 'private'",
    )?;
    ensure_column(&conn, "trips", "pin_hash", "TEXT NOT NULL DEFAULT ''")?;
    ensure_column(&conn, "itinerary_items", "end_day", "TEXT")?;
    ensure_column(&conn, "itinerary_items", "parent_id", "INTEGER")?;
    ensure_column(
        &conn,
        "itinerary_items",
        "kind",
        "TEXT NOT NULL DEFAULT 'stop'",
    )?;
    conn.execute(
        "UPDATE trips SET edit_key=lower(hex(randomblob(16))) WHERE edit_key=''",
        [],
    )?;
    Ok(())
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = statement.query_map([], |row| row.get::<_, String>(1))?;
    let exists = names
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition}"
        ))?;
    }
    Ok(())
}

#[derive(Deserialize)]
struct NewTrip {
    title: String,
    start_date: String,
    end_date: String,
    travelers: Option<i64>,
    currency: Option<String>,
    origin: Option<String>,
    traveler_names: Option<String>,
    emergency_name: Option<String>,
    emergency_phone: Option<String>,
    share_mode: Option<String>,
    pin: Option<String>,
}

#[derive(Deserialize)]
struct UpdateTrip {
    title: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    travelers: Option<i64>,
    currency: Option<String>,
    origin: Option<String>,
    traveler_names: Option<String>,
    emergency_name: Option<String>,
    emergency_phone: Option<String>,
    share_mode: Option<String>,
    pin: Option<String>,
}

#[derive(Deserialize)]
struct NewItem {
    start_date: String,
    end_date: Option<String>,
    parent_id: Option<i64>,
    kind: Option<String>,
    time: Option<String>,
    title: String,
    place: Option<String>,
    category: Option<String>,
    notes: Option<String>,
    cost: Option<f64>,
    latitude: Option<f64>,
    longitude: Option<f64>,
}

#[derive(Deserialize)]
struct UpdateItem {
    start_date: Option<String>,
    end_date: Option<String>,
    kind: Option<String>,
    time: Option<String>,
    title: Option<String>,
    place: Option<String>,
    category: Option<String>,
    notes: Option<String>,
    cost: Option<f64>,
    latitude: Option<f64>,
    longitude: Option<f64>,
    #[serde(default)]
    clear_coordinates: bool,
}

#[derive(Deserialize)]
struct NewComment {
    name: String,
    body: String,
}

#[derive(Deserialize)]
struct NewExpense {
    description: String,
    amount: f64,
    category: Option<String>,
}

#[derive(Serialize)]
struct TripData {
    trip: Value,
    items: Vec<Value>,
    comments: Vec<Value>,
    expenses: Vec<Value>,
    photos: Vec<Value>,
    access: String,
}

#[derive(Deserialize)]
struct NewPhoto {
    data_url: String,
    caption: Option<String>,
}

#[derive(Deserialize)]
struct PlaceQuery {
    q: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct PlaceResult {
    place_id: u64,
    display_name: String,
    lat: String,
    lon: String,
    #[serde(default)]
    namedetails: Option<std::collections::HashMap<String, String>>,
}

async fn home() -> Html<&'static str> {
    Html(include_str!("../static/index.html"))
}

async fn create_trip(
    State(state): State<AppState>,
    Json(input): Json<NewTrip>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if input.title.trim().is_empty() || input.start_date.is_empty() || input.end_date.is_empty() {
        return Err(bad("Title and travel dates are required"));
    }
    if input.end_date < input.start_date {
        return Err(bad("End date must be on or after the start date"));
    }
    let share_mode = input.share_mode.unwrap_or_else(|| "private".into());
    if !valid_share_mode(&share_mode) {
        return Err(bad("Sharing must be private, PIN protected, or public"));
    }
    let pin_hash = match (share_mode.as_str(), input.pin.as_deref()) {
        ("pin", Some(pin)) if valid_pin(pin) => hash_pin(pin)?,
        ("pin", _) => return Err(bad("Set a four-digit PIN for PIN-protected sharing")),
        (_, Some(pin)) if !pin.is_empty() => {
            return Err(bad("A PIN is only used with PIN-protected sharing"))
        }
        _ => String::new(),
    };
    let id = Uuid::new_v4().to_string();
    let edit_key = Uuid::new_v4().to_string();
    let created = now();
    let trip_id = id.clone();
    let title = input.title.trim().to_owned();
    let start = input.start_date;
    let end = input.end_date;
    let travelers = input.travelers.unwrap_or(1).max(1);
    let currency = input.currency.unwrap_or_else(|| "AUD".into());
    let origin = input.origin.unwrap_or_default();
    let traveler_names = input.traveler_names.unwrap_or_default();
    let emergency_name = input.emergency_name.unwrap_or_default();
    let emergency_phone = input.emergency_phone.unwrap_or_default();
    let new_edit_key = edit_key.clone();
    db(&state, move |conn| {
        conn.execute("INSERT INTO trips (id,title,start_date,end_date,travelers,currency,origin,traveler_names,emergency_name,emergency_phone,edit_key,share_mode,pin_hash,created_at,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14)", params![trip_id,title,start,end,travelers,currency,origin,traveler_names,emergency_name,emergency_phone,new_edit_key,share_mode,pin_hash,created]).map_err(internal)?;
        Ok(())
    }).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":id,"edit_key":edit_key})),
    ))
}

async fn get_trip(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<Json<TripData>> {
    let id_for_access = id.clone();
    let (share_mode, pin_hash, edit_key, end_date): (String, String, String, String) =
        db(&state, move |conn| {
            conn.query_row(
                "SELECT share_mode,pin_hash,edit_key,end_date FROM trips WHERE id=?1",
                [&id_for_access],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(not_found)
        })
        .await?;
    let supplied_edit_key = headers
        .get("x-edit-key")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let is_editor = !edit_key.is_empty() && supplied_edit_key == edit_key;
    if share_mode == "public" && !is_editor && end_date < Utc::now().date_naive().to_string() {
        return Err(not_found());
    }
    let supplied_pin = headers
        .get("x-trip-pin")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    if share_mode == "pin" && !is_editor {
        if supplied_pin.is_empty() {
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "Enter this trip's four-digit PIN".into(),
            ));
        }
        let attempt_keys = pin_attempt_keys(&id, &headers);
        let now_instant = Instant::now();
        {
            let mut attempts = state.pin_attempts.lock().await;
            if attempts.blocked(&attempt_keys, now_instant) {
                return Err(ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    "Too many incorrect PIN attempts. Try again in 15 minutes.".into(),
                ));
            }
        }
        let pin = supplied_pin.to_owned();
        let valid = tokio::task::spawn_blocking(move || verify_pin(&pin, &pin_hash))
            .await
            .map_err(internal)?;
        if !valid {
            state
                .pin_attempts
                .lock()
                .await
                .fail(&attempt_keys, now_instant);
            return Err(ApiError(
                StatusCode::UNAUTHORIZED,
                "That PIN is incorrect".into(),
            ));
        }
        state.pin_attempts.lock().await.clear(&attempt_keys);
    }
    let limited_view = !is_editor && matches!(share_mode.as_str(), "public" | "pin");
    let access = if is_editor {
        "editor"
    } else if share_mode == "public" {
        "public"
    } else if share_mode == "pin" {
        "pin"
    } else {
        "family"
    }
    .to_owned();
    db(&state, move |conn| {
        let trip_sql = if limited_view {
            "SELECT json_object('id',id,'title',title,'start_date',start_date,'end_date',end_date) FROM trips WHERE id=?1"
        } else {
            "SELECT json_object('id',id,'title',title,'start_date',start_date,'end_date',end_date,'travelers',travelers,'currency',currency,'origin',origin,'traveler_names',traveler_names,'emergency_name',emergency_name,'emergency_phone',emergency_phone,'share_mode',share_mode,'updated_at',updated_at) FROM trips WHERE id=?1"
        };
        let trip: Option<String> = conn.query_row(trip_sql, [&id], |r| r.get(0)).optional().map_err(internal)?;
        let trip = serde_json::from_str(&trip.ok_or_else(not_found)?).map_err(internal)?;
        let read_rows = |sql: &str| -> ApiResult<Vec<Value>> {
            let mut stmt = conn.prepare(sql).map_err(internal)?;
            let rows = stmt.query_map([&id], |r| r.get::<_, String>(0)).map_err(internal)?;
            rows.map(|r| r.map_err(internal).and_then(|s| serde_json::from_str(&s).map_err(internal))).collect()
        };
        let item_sql = if limited_view {
            "SELECT json_object('id',id,'day',day,'start_date',day,'end_date',COALESCE(end_day,day),'parent_id',parent_id,'kind',kind,'time',time,'title',title,'place',place,'category',category,'latitude',latitude,'longitude',longitude) FROM itinerary_items WHERE trip_id=?1 ORDER BY day,time,id"
        } else {
            "SELECT json_object('id',id,'day',day,'start_date',day,'end_date',COALESCE(end_day,day),'parent_id',parent_id,'kind',kind,'time',time,'title',title,'place',place,'category',category,'notes',notes,'cost',cost,'latitude',latitude,'longitude',longitude) FROM itinerary_items WHERE trip_id=?1 ORDER BY day,time,id"
        };
        let items = read_rows(item_sql)?;
        let (comments, expenses, photos) = if limited_view {
            (Vec::new(), Vec::new(), Vec::new())
        } else {
            (
                read_rows("SELECT json_object('id',id,'name',name,'body',body,'created_at',created_at) FROM comments WHERE trip_id=?1 ORDER BY id DESC")?,
                read_rows("SELECT json_object('id',id,'description',description,'amount',amount,'category',category) FROM expenses WHERE trip_id=?1 ORDER BY id")?,
                read_rows("SELECT json_object('id',id,'url','/photos/'||trip_id||'/'||filename,'caption',caption,'created_at',created_at) FROM photos WHERE trip_id=?1 ORDER BY id DESC")?,
            )
        };
        Ok(Json(TripData { trip, items, comments, expenses, photos, access }))
    }).await
}

async fn public_trips(State(state): State<AppState>) -> ApiResult<Json<Vec<Value>>> {
    let today = Utc::now().date_naive().to_string();
    db(&state, move |conn| {
        let mut statement = conn.prepare(
            "SELECT json_object('id',id,'title',title,'start_date',start_date,'end_date',end_date) FROM trips WHERE share_mode='public' AND end_date>=?1 ORDER BY start_date,title",
        ).map_err(internal)?;
        let rows = statement
            .query_map([today], |row| row.get::<_, String>(0))
            .map_err(internal)?;
        rows.map(|row| {
            let json = row.map_err(internal)?;
            serde_json::from_str(&json).map_err(internal)
        })
        .collect::<ApiResult<Vec<_>>>()
    })
    .await
    .map(Json)
}

async fn update_trip(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<UpdateTrip>,
) -> ApiResult<Json<Value>> {
    require_editor(&state, &id, &headers).await?;
    if let Some(mode) = input.share_mode.as_deref() {
        if !valid_share_mode(mode) {
            return Err(bad("Sharing must be private, PIN protected, or public"));
        }
    }
    db(&state, move |conn| {
        let existing: Option<(String, String)> = conn.query_row("SELECT share_mode,pin_hash FROM trips WHERE id=?1", [&id], |row| Ok((row.get(0)?,row.get(1)?))).optional().map_err(internal)?;
        let (existing_mode, existing_hash) = existing.ok_or_else(not_found)?;
        if let (Some(a), Some(b)) = (&input.start_date, &input.end_date) { if b < a { return Err(bad("End date must be on or after the start date")); } }
        let share_mode = input.share_mode.unwrap_or(existing_mode.clone());
        let pin_hash = if share_mode == "pin" {
            match input.pin.as_deref().filter(|pin| !pin.is_empty()) {
                Some(pin) if valid_pin(pin) => hash_pin(pin)?,
                Some(_) => return Err(bad("PIN must be exactly four digits")),
                None if existing_mode == "pin" && !existing_hash.is_empty() => existing_hash,
                None => return Err(bad("Set a four-digit PIN for PIN-protected sharing")),
            }
        } else {
            String::new()
        };
        conn.execute("UPDATE trips SET title=COALESCE(?2,title),start_date=COALESCE(?3,start_date),end_date=COALESCE(?4,end_date),travelers=COALESCE(?5,travelers),currency=COALESCE(?6,currency),origin=COALESCE(?7,origin),traveler_names=COALESCE(?8,traveler_names),emergency_name=COALESCE(?9,emergency_name),emergency_phone=COALESCE(?10,emergency_phone),share_mode=?11,pin_hash=?12,updated_at=?13 WHERE id=?1", params![id,input.title,input.start_date,input.end_date,input.travelers,input.currency,input.origin,input.traveler_names,input.emergency_name,input.emergency_phone,share_mode,pin_hash,now()]).map_err(internal)?;
        Ok(Json(json!({"ok":true})))
    }).await
}

async fn add_item(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NewItem>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if input.title.trim().is_empty() || input.start_date.is_empty() {
        return Err(bad("A start date and title are required"));
    }
    let end_date = input
        .end_date
        .clone()
        .unwrap_or_else(|| input.start_date.clone());
    if end_date < input.start_date {
        return Err(bad("End date must be on or after the start date"));
    }
    if let Some(parent_id) = input.parent_id {
        let parent_trip = id.clone();
        let belongs = db(&state, move |conn| conn.query_row("SELECT EXISTS(SELECT 1 FROM itinerary_items WHERE id=?1 AND trip_id=?2 AND parent_id IS NULL)", params![parent_id,parent_trip], |r| r.get::<_,bool>(0)).map_err(internal)).await?;
        if !belongs {
            return Err(bad("Activity must belong to a stop in this trip"));
        }
    }
    require_editor(&state, &id, &headers).await?;
    db(&state, move |conn| {
        let result = conn.execute("INSERT INTO itinerary_items (trip_id,day,end_day,parent_id,kind,time,title,place,category,notes,cost,latitude,longitude) SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13 WHERE EXISTS(SELECT 1 FROM trips WHERE id=?1)", params![id,input.start_date,end_date,input.parent_id,input.kind.unwrap_or_else(||"stop".into()),input.time.unwrap_or_default(),input.title.trim(),input.place.unwrap_or_default(),input.category.unwrap_or_else(||"activity".into()),input.notes.unwrap_or_default(),input.cost.unwrap_or(0.0),input.latitude,input.longitude]).map_err(internal)?;
        if result == 0 { return Err(not_found()); }
        Ok((StatusCode::CREATED, Json(json!({"id":conn.last_insert_rowid()}))))
    }).await
}

async fn delete_item(
    State(state): State<AppState>,
    Path((trip, item)): Path<(String, i64)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_editor(&state, &trip, &headers).await?;
    db(&state, move |conn| {
        conn.execute(
            "DELETE FROM itinerary_items WHERE trip_id=?1 AND parent_id=?2",
            params![trip, item],
        )
        .map_err(internal)?;
        conn.execute(
            "DELETE FROM itinerary_items WHERE id=?1 AND trip_id=?2",
            params![item, trip],
        )
        .map_err(internal)?;
        Ok(Json(json!({"ok":true})))
    })
    .await
}

async fn update_item(
    State(state): State<AppState>,
    Path((trip, item)): Path<(String, i64)>,
    headers: HeaderMap,
    Json(input): Json<UpdateItem>,
) -> ApiResult<Json<Value>> {
    require_editor(&state, &trip, &headers).await?;
    db(&state, move |conn| {
        let current: Option<(String, String, Option<i64>)> = conn
            .query_row(
                "SELECT day,COALESCE(end_day,day),parent_id FROM itinerary_items WHERE id=?1 AND trip_id=?2",
                params![item, trip],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(internal)?;
        let (current_start, current_end, parent_id) = current.ok_or_else(not_found)?;
        let start = input.start_date.as_deref().unwrap_or(&current_start);
        let end = input.end_date.as_deref().unwrap_or(&current_end);
        if end < start {
            return Err(bad("End date must be on or after the start date"));
        }
        if let Some(parent_id) = parent_id {
            let parent_range: (String, String) = conn
                .query_row(
                    "SELECT day,COALESCE(end_day,day) FROM itinerary_items WHERE id=?1 AND trip_id=?2 AND parent_id IS NULL",
                    params![parent_id, trip],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(internal)?;
            if start < parent_range.0.as_str() || end > parent_range.1.as_str() {
                return Err(bad("Activity dates must stay within their journey stop"));
            }
        } else {
            let outside_activity: bool = conn
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM itinerary_items WHERE trip_id=?1 AND parent_id=?2 AND (day<?3 OR COALESCE(end_day,day)>?4))",
                    params![trip, item, start, end],
                    |r| r.get(0),
                )
                .map_err(internal)?;
            if outside_activity {
                return Err(bad("Extend this stop to include its existing activities first"));
            }
        }
        conn.execute(
            "UPDATE itinerary_items SET day=?3,end_day=?4,kind=COALESCE(?5,kind),time=COALESCE(?6,time),title=COALESCE(?7,title),place=COALESCE(?8,place),category=COALESCE(?9,category),notes=COALESCE(?10,notes),cost=COALESCE(?11,cost),latitude=CASE WHEN ?12 THEN NULL WHEN ?13 IS NOT NULL THEN ?13 ELSE latitude END,longitude=CASE WHEN ?12 THEN NULL WHEN ?14 IS NOT NULL THEN ?14 ELSE longitude END WHERE id=?1 AND trip_id=?2",
            params![item,trip,start,end,input.kind,input.time,input.title,input.place,input.category,input.notes,input.cost,input.clear_coordinates,input.latitude,input.longitude],
        ).map_err(internal)?;
        Ok(Json(json!({"ok":true})))
    })
    .await
}

async fn add_comment(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<NewComment>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if input.name.trim().is_empty() || input.body.trim().is_empty() {
        return Err(bad("Name and comment are required"));
    }
    let share_mode = db(&state, {
        let trip_id = id.clone();
        move |conn| {
            conn.query_row(
                "SELECT share_mode FROM trips WHERE id=?1",
                [&trip_id],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(internal)?
            .ok_or_else(not_found)
        }
    })
    .await?;
    if share_mode != "private" {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "Comments are only available through the private family link".into(),
        ));
    }
    db(&state, move |conn| {
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM trips WHERE id=?1", [&id], |r| {
                r.get(0)
            })
            .map_err(internal)?;
        if count == 0 {
            return Err(not_found());
        }
        conn.execute(
            "INSERT INTO comments(trip_id,name,body,created_at) VALUES (?1,?2,?3,?4)",
            params![id, input.name.trim(), input.body.trim(), now()],
        )
        .map_err(internal)?;
        Ok((
            StatusCode::CREATED,
            Json(json!({"id":conn.last_insert_rowid()})),
        ))
    })
    .await
}

async fn add_expense(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NewExpense>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if input.description.trim().is_empty() || input.amount < 0.0 {
        return Err(bad("A description and non-negative amount are required"));
    }
    require_editor(&state, &id, &headers).await?;
    db(&state, move |conn| {
        let result = conn.execute("INSERT INTO expenses(trip_id,description,amount,category) SELECT ?1,?2,?3,?4 WHERE EXISTS(SELECT 1 FROM trips WHERE id=?1)", params![id,input.description.trim(),input.amount,input.category.unwrap_or_else(||"Other".into())]).map_err(internal)?;
        if result == 0 { return Err(not_found()); }
        Ok((StatusCode::CREATED, Json(json!({"id":conn.last_insert_rowid()}))))
    }).await
}

async fn delete_expense(
    State(state): State<AppState>,
    Path((trip, expense)): Path<(String, i64)>,
    headers: HeaderMap,
) -> ApiResult<Json<Value>> {
    require_editor(&state, &trip, &headers).await?;
    db(&state, move |conn| {
        conn.execute(
            "DELETE FROM expenses WHERE id=?1 AND trip_id=?2",
            params![expense, trip],
        )
        .map_err(internal)?;
        Ok(Json(json!({"ok":true})))
    })
    .await
}

async fn clone_trip(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_editor(&state, &id, &headers).await?;
    let new_id = Uuid::new_v4().to_string();
    let edit_key = Uuid::new_v4().to_string();
    let copy_id = new_id.clone();
    let copy_key = edit_key.clone();
    db(&state, move |conn| {
        let changed = conn.execute("INSERT INTO trips(id,title,start_date,end_date,travelers,currency,origin,traveler_names,emergency_name,emergency_phone,edit_key,created_at,updated_at) SELECT ?2,'Copy of '||title,start_date,end_date,travelers,currency,origin,traveler_names,emergency_name,emergency_phone,?3,?4,?4 FROM trips WHERE id=?1", params![id,copy_id,copy_key,now()]).map_err(internal)?;
        if changed == 0 { return Err(not_found()); }
        let items = {
            let mut statement = conn.prepare("SELECT id,day,end_day,time,title,place,category,notes,cost,latitude,longitude,kind,parent_id FROM itinerary_items WHERE trip_id=?1 ORDER BY parent_id IS NOT NULL,id").map_err(internal)?;
            let rows = statement.query_map([&id], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?,r.get::<_,String>(6)?,r.get::<_,String>(7)?,r.get::<_,f64>(8)?,r.get::<_,Option<f64>>(9)?,r.get::<_,Option<f64>>(10)?,r.get::<_,String>(11)?,r.get::<_,Option<i64>>(12)?))).map_err(internal)?;
            rows.collect::<Result<Vec<_>,_>>().map_err(internal)?
        };
        let mut copied = std::collections::HashMap::new();
        for (old_id,day,end_day,time,title,place,category,notes,cost,latitude,longitude,kind,parent_id) in items {
            let new_parent = parent_id.and_then(|parent| copied.get(&parent).copied());
            conn.execute("INSERT INTO itinerary_items(trip_id,day,end_day,parent_id,kind,time,title,place,category,notes,cost,latitude,longitude) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![copy_id,day,end_day,new_parent,kind,time,title,place,category,notes,cost,latitude,longitude]).map_err(internal)?;
            copied.insert(old_id,conn.last_insert_rowid());
        }
        conn.execute("INSERT INTO expenses(trip_id,description,amount,category) SELECT ?2,description,amount,category FROM expenses WHERE trip_id=?1", params![id,copy_id]).map_err(internal)?;
        Ok((StatusCode::CREATED, Json(json!({"id":copy_id,"edit_key":edit_key}))))
    }).await
}

async fn add_photo(
    State(state): State<AppState>,
    Path(id): Path<String>,
    headers: HeaderMap,
    Json(input): Json<NewPhoto>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    require_editor(&state, &id, &headers).await?;
    let (_, encoded) = input
        .data_url
        .split_once(',')
        .ok_or_else(|| bad("Upload a JPEG image"))?;
    if !input.data_url.starts_with("data:image/jpeg;base64,") || encoded.len() > 1_500_000 {
        return Err(bad("Photos must be compressed JPEG images under 1 MB"));
    }
    let bytes = STANDARD.decode(encoded).map_err(|e| bad(&e.to_string()))?;
    let trip_id = Uuid::parse_str(&id)
        .map_err(|_| bad("Invalid trip link"))?
        .to_string();
    let photo_dir = state.photo_path.join(&trip_id);
    let filename = format!("{}.jpg", Uuid::new_v4());
    std::fs::create_dir_all(&photo_dir).map_err(internal)?;
    std::fs::write(photo_dir.join(&filename), bytes).map_err(internal)?;
    let stored_name = filename.clone();
    let caption = input.caption.unwrap_or_default();
    let photo_id = db(&state, move |conn| {
        conn.execute("INSERT INTO photos(trip_id,filename,caption,created_at) SELECT ?1,?2,?3,?4 WHERE EXISTS(SELECT 1 FROM trips WHERE id=?1)", params![trip_id,stored_name,caption,now()]).map_err(internal)?;
        Ok(conn.last_insert_rowid())
    }).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"id":photo_id,"url":format!("/photos/{id}/{filename}")})),
    ))
}

async fn search_places(
    State(state): State<AppState>,
    Query(query): Query<PlaceQuery>,
) -> ApiResult<Json<Vec<PlaceResult>>> {
    let search = query.q.trim().to_owned();
    if search.len() < 3 {
        return Err(bad("Enter at least three characters to search"));
    }
    if search.len() > 200 {
        return Err(bad("Place search is too long"));
    }
    let cache_key = format!("v2:{}", search.to_lowercase());
    let lookup_key = cache_key.clone();
    if let Some(cached) = db(&state, move |conn| {
        conn.query_row(
            "SELECT results FROM geocode_cache WHERE query=?1",
            [&lookup_key],
            |r| r.get::<_, String>(0),
        )
        .optional()
        .map_err(internal)
    })
    .await?
    {
        return serde_json::from_str(&cached).map(Json).map_err(internal);
    }

    // Nominatim asks each application to stay below one request per second.
    let mut last = state.geocoder_last_request.lock().await;
    if let Some(previous) = *last {
        let remaining = Duration::from_secs(1).saturating_sub(previous.elapsed());
        if !remaining.is_zero() {
            tokio::time::sleep(remaining).await;
        }
    }
    *last = Some(Instant::now());
    drop(last);

    let response = state
        .geocoder_client
        .get(state.geocoder_url.as_ref())
        .query(&[
            ("q", search.as_str()),
            ("format", "jsonv2"),
            ("limit", "6"),
            ("accept-language", "en"),
            ("namedetails", "1"),
        ])
        .send()
        .await
        .map_err(|e| {
            ApiError(
                StatusCode::BAD_GATEWAY,
                format!("Place search is temporarily unavailable: {e}"),
            )
        })?
        .error_for_status()
        .map_err(|e| {
            ApiError(
                StatusCode::BAD_GATEWAY,
                format!("Place search is temporarily unavailable: {e}"),
            )
        })?;
    let results = response.json::<Vec<PlaceResult>>().await.map_err(|e| {
        ApiError(
            StatusCode::BAD_GATEWAY,
            format!("Could not read place search results: {e}"),
        )
    })?;
    let results_json = serde_json::to_string(&results).map_err(internal)?;
    let insert_key = cache_key;
    let insert_results = results_json;
    db(&state, move |conn| {
        conn.execute("INSERT INTO geocode_cache(query,results,updated_at) VALUES (?1,?2,?3) ON CONFLICT(query) DO UPDATE SET results=excluded.results,updated_at=excluded.updated_at", params![insert_key,insert_results,now()]).map_err(internal)?;
        Ok(())
    }).await?;
    Ok(Json(results))
}

async fn map_config() -> Json<Value> {
    Json(json!({
        "style_url": env::var("MAP_STYLE_URL")
            .unwrap_or_else(|_| "https://tiles.openfreemap.org/styles/liberty".into()),
        "public_url": env::var("TRIP_PUBLIC_URL").unwrap_or_default().trim_end_matches('/'),
    }))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = PathBuf::from(env::var("TRIP_DB").unwrap_or_else(|_| "data/trips.sqlite".into()));
    init_db(&db_path)?;
    let photo_path = db_path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."))
        .join("photos");
    let state = AppState {
        db_path: Arc::new(db_path),
        photo_path: Arc::new(photo_path.clone()),
        geocoder_last_request: Arc::new(tokio::sync::Mutex::new(None)),
        geocoder_url: Arc::new(
            env::var("GEOCODER_URL")
                .unwrap_or_else(|_| "https://nominatim.openstreetmap.org/search".into()),
        ),
        geocoder_client: Arc::new(
            reqwest::Client::builder()
                .user_agent("WayfarerTripPlanner/0.1 (small group trip planner)")
                .build()?,
        ),
        pin_attempts: Arc::new(tokio::sync::Mutex::new(PinAttempts::default())),
    };
    let app = Router::new()
        .route("/", get(home))
        .route("/api/trips", post(create_trip))
        .route("/api/public/trips", get(public_trips))
        .route("/api/trips/:id", get(get_trip).patch(update_trip))
        .route("/api/trips/:id/items", post(add_item))
        .route(
            "/api/trips/:id/items/:item",
            axum::routing::patch(update_item).delete(delete_item),
        )
        .route("/api/trips/:id/comments", post(add_comment))
        .route("/api/trips/:id/photos", post(add_photo))
        .route("/api/places", get(search_places))
        .route("/api/map-config", get(map_config))
        .route("/api/trips/:id/expenses", post(add_expense))
        .route("/api/trips/:id/expenses/:expense", delete(delete_expense))
        .route("/api/trips/:id/copy", post(clone_trip))
        .nest_service("/static", ServeDir::new("static"))
        .nest_service("/photos", ServeDir::new(photo_path))
        .with_state(state);
    let address = env::var("TRIP_ADDR").unwrap_or_else(|_| "127.0.0.1:3007".into());
    let listener = tokio::net::TcpListener::bind(&address).await?;
    println!("Trip planner ready at http://{address}");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{valid_pin, PinAttempts};
    use std::time::{Duration, Instant};

    #[test]
    fn pin_must_be_exactly_four_ascii_digits() {
        assert!(valid_pin("0427"));
        assert!(!valid_pin("427"));
        assert!(!valid_pin("04270"));
        assert!(!valid_pin("0４27"));
        assert!(!valid_pin("abcd"));
    }

    #[test]
    fn pin_attempt_limit_expires_after_fifteen_minutes() {
        let mut attempts = PinAttempts::default();
        let keys = vec![
            "ip:192.0.2.1".to_owned(),
            "browser:192.0.2.1:agent".to_owned(),
        ];
        let now = Instant::now();
        for _ in 0..5 {
            attempts.fail(&keys, now);
        }
        assert!(attempts.blocked(&keys, now));
        assert!(!attempts.blocked(&keys, now + Duration::from_secs(15 * 60)));
    }
}
