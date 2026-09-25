use std::collections::HashMap;

use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use zbus::Message;

use crate::desktop::{DesktopError, Frame};
use crate::input::{Button, MouseOp, Stroke};

const PORTAL_DEST: &str = "org.freedesktop.portal.Desktop";
const PORTAL_PATH: &str = "/org/freedesktop/portal/desktop";
const REMOTE: &str = "org.freedesktop.portal.RemoteDesktop";
const SHOT: &str = "org.freedesktop.portal.Screenshot";
const REQUEST: &str = "org.freedesktop.portal.Request";
const DEVICE_KEYBOARD: u32 = 1;
const DEVICE_POINTER: u32 = 2;

pub struct WaylandInput {
    conn: Connection,
    session_handle: String,
}

impl WaylandInput {
    pub fn start() -> Result<Self, DesktopError> {
        let conn = session_bus()?;
        let sender = sender_path_token(&conn)?;
        let handle_token = path_token("hc");
        let session_token = path_token("hs");
        let options = token_options(
            &handle_token,
            vec![("session_handle_token", Value::from(session_token.as_str()))],
        );
        let created = portal_roundtrip(
            &conn,
            &sender,
            REMOTE,
            "CreateSession",
            &handle_token,
            &options,
        )?;
        let session_handle = required_string(&created, "session_handle")?;
        let session = object_path(&session_handle)?;

        let handle_token = path_token("hd");
        let options = token_options(
            &handle_token,
            vec![("types", Value::from(DEVICE_KEYBOARD | DEVICE_POINTER))],
        );
        portal_roundtrip(
            &conn,
            &sender,
            REMOTE,
            "SelectDevices",
            &handle_token,
            &(&session, &options),
        )?;

        let handle_token = path_token("ht");
        let options = token_options(&handle_token, Vec::new());
        portal_roundtrip(
            &conn,
            &sender,
            REMOTE,
            "Start",
            &handle_token,
            &(&session, "", &options),
        )?;
        Ok(Self {
            conn,
            session_handle,
        })
    }

    pub fn mouse(&self, op: &MouseOp) -> Result<(), DesktopError> {
        match *op {
            MouseOp::Move { x, y } => self.move_abs(x, y),
            MouseOp::Click { x, y, button } => {
                self.move_abs(x, y)?;
                self.button(button, true)?;
                self.button(button, false)
            }
            MouseOp::Drag {
                x,
                y,
                to_x,
                to_y,
                button,
            } => {
                self.move_abs(x, y)?;
                self.button(button, true)?;
                self.move_abs(to_x, to_y)?;
                self.button(button, false)
            }
        }
    }

    pub fn type_strokes(&self, strokes: &[Stroke]) -> Result<(), DesktopError> {
        for stroke in strokes {
            self.keysym(stroke.keysym, stroke.down)?;
        }
        Ok(())
    }

    fn move_abs(&self, x: i32, y: i32) -> Result<(), DesktopError> {
        let session = object_path(&self.session_handle)?;
        let options: HashMap<&str, Value> = HashMap::new();
        self.remote()?
            .call::<_, _, ()>(
                "NotifyPointerMotionAbsolute",
                &(session, &options, 0u32, f64::from(x), f64::from(y)),
            )
            .map_err(input_err)
    }

    fn button(&self, button: Button, down: bool) -> Result<(), DesktopError> {
        let session = object_path(&self.session_handle)?;
        let options: HashMap<&str, Value> = HashMap::new();
        let state = if down { 1u32 } else { 0u32 };
        self.remote()?
            .call::<_, _, ()>(
                "NotifyPointerButton",
                &(session, &options, evdev_button(button), state),
            )
            .map_err(input_err)
    }

    fn keysym(&self, keysym: u32, down: bool) -> Result<(), DesktopError> {
        let session = object_path(&self.session_handle)?;
        let options: HashMap<&str, Value> = HashMap::new();
        let state = if down { 1u32 } else { 0u32 };
        self.remote()?
            .call::<_, _, ()>(
                "NotifyKeyboardKeysym",
                &(session, &options, keysym as i32, state),
            )
            .map_err(input_err)
    }

    fn remote(&self) -> Result<Proxy<'_>, DesktopError> {
        Proxy::new(&self.conn, PORTAL_DEST, PORTAL_PATH, REMOTE).map_err(input_err)
    }
}

impl Drop for WaylandInput {
    fn drop(&mut self) {
        if let Ok(path) = object_path(&self.session_handle) {
            if let Ok(proxy) = Proxy::new(
                &self.conn,
                PORTAL_DEST,
                path,
                "org.freedesktop.portal.Session",
            ) {
                let _ = proxy.call::<_, _, ()>("Close", &());
            }
        }
    }
}

pub fn capture() -> Result<Frame, DesktopError> {
    let conn = session_bus()?;
    let sender = sender_path_token(&conn)?;
    let token = path_token("hp");
    let options = token_options(&token, vec![("interactive", Value::from(false))]);
    let results = portal_roundtrip(&conn, &sender, SHOT, "Screenshot", &token, &("", &options))?;
    let uri = required_string(&results, "uri")?;
    let bytes = read_local_uri(&uri)?;
    if bytes.starts_with(b"\x89PNG") {
        return Ok(Frame::png(bytes));
    }
    Err(DesktopError::Unavailable(
        "wayland screenshot was not a png".into(),
    ))
}

pub(crate) fn read_local_uri(uri: &str) -> Result<Vec<u8>, DesktopError> {
    let path = file_uri_path(uri)?;
    let bytes = std::fs::read(&path)
        .map_err(|err| DesktopError::Unavailable(format!("screenshot file: {err}")))?;
    std::fs::remove_file(&path)
        .map_err(|err| DesktopError::Unavailable(format!("screenshot file: {err}")))?;
    Ok(bytes)
}

pub(crate) fn file_uri_path(uri: &str) -> Result<std::path::PathBuf, DesktopError> {
    let rest = uri
        .strip_prefix("file://")
        .ok_or_else(|| DesktopError::Unavailable("screenshot uri is not a local file".into()))?;
    let (host, path) = if let Some(stripped) = rest.strip_prefix('/') {
        ("", stripped)
    } else {
        let (host, path) = rest.split_once('/').ok_or_else(|| {
            DesktopError::Unavailable("screenshot uri is not a local file".into())
        })?;
        (host, path)
    };
    if !host.is_empty() && host != "localhost" {
        return Err(DesktopError::Unavailable(
            "screenshot uri is not a local file".into(),
        ));
    }
    let decoded = percent_decode(path)?;
    let full = format!("/{decoded}");
    let path = std::path::PathBuf::from(&full);
    if !path.is_absolute() {
        return Err(DesktopError::Unavailable(
            "screenshot uri is not a local file".into(),
        ));
    }
    Ok(path)
}

fn percent_decode(value: &str) -> Result<String, DesktopError> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(DesktopError::Unavailable(
                    "screenshot uri is not a local file".into(),
                ));
            }
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).map_err(|_| {
                DesktopError::Unavailable("screenshot uri is not a local file".into())
            })?;
            let byte = u8::from_str_radix(hex, 16).map_err(|_| {
                DesktopError::Unavailable("screenshot uri is not a local file".into())
            })?;
            out.push(byte);
            index += 3;
            continue;
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8(out)
        .map_err(|_| DesktopError::Unavailable("screenshot uri is not a local file".into()))
}

fn session_bus() -> Result<Connection, DesktopError> {
    Connection::session()
        .map_err(|err| DesktopError::Unavailable(format!("wayland session bus: {err}")))
}

fn portal_roundtrip<B>(
    conn: &Connection,
    sender: &str,
    interface: &'static str,
    method: &'static str,
    token: &str,
    body: &B,
) -> Result<HashMap<String, OwnedValue>, DesktopError>
where
    B: serde::ser::Serialize + zbus::zvariant::DynamicType,
{
    let request_path = format!("/org/freedesktop/portal/desktop/request/{sender}/{token}");
    let request =
        Proxy::new(conn, PORTAL_DEST, request_path.as_str(), REQUEST).map_err(unavailable)?;
    let mut signals = request.receive_signal("Response").map_err(unavailable)?;
    let portal = Proxy::new(conn, PORTAL_DEST, PORTAL_PATH, interface).map_err(unavailable)?;
    let returned: OwnedObjectPath = portal.call(method, body).map_err(unavailable)?;
    if returned.as_str() != request_path {
        return Err(DesktopError::Unavailable(format!(
            "wayland portal request path mismatch: {}",
            returned.as_str()
        )));
    }
    let message = signals
        .next()
        .ok_or_else(|| DesktopError::Unavailable("wayland portal closed the request".into()))?;
    let (code, results): (u32, HashMap<String, OwnedValue>) = message_body(&message)?;
    if code != 0 {
        return Err(DesktopError::Unavailable(format!(
            "wayland portal response code {code}"
        )));
    }
    Ok(results)
}

fn message_body(message: &Message) -> Result<(u32, HashMap<String, OwnedValue>), DesktopError> {
    message
        .body()
        .deserialize()
        .map_err(|err| DesktopError::Unavailable(format!("wayland portal response: {err}")))
}

pub(crate) fn bus_name_to_path_token(unique: &str) -> String {
    unique.trim_start_matches(':').replace('.', "_")
}

fn sender_path_token(conn: &Connection) -> Result<String, DesktopError> {
    let name = conn
        .unique_name()
        .ok_or_else(|| DesktopError::Unavailable("wayland bus name is missing".into()))?;
    Ok(bus_name_to_path_token(name.as_str()))
}

fn token_options<'a>(
    token: &'a str,
    extra: Vec<(&'a str, Value<'a>)>,
) -> HashMap<&'a str, Value<'a>> {
    let mut options = HashMap::new();
    options.insert("handle_token", Value::from(token));
    for (key, value) in extra {
        options.insert(key, value);
    }
    options
}

fn path_token(prefix: &str) -> String {
    format!("{prefix}{}", uuid::Uuid::new_v4().simple())
}

fn required_string(
    results: &HashMap<String, OwnedValue>,
    key: &str,
) -> Result<String, DesktopError> {
    let value = results
        .get(key)
        .ok_or_else(|| DesktopError::Unavailable(format!("wayland portal result missing {key}")))?;
    owned_string(value).ok_or_else(|| {
        DesktopError::Unavailable(format!(
            "wayland portal result {key} has an unexpected type"
        ))
    })
}

fn owned_string(value: &OwnedValue) -> Option<String> {
    match &**value {
        Value::Str(text) => Some(text.as_str().to_string()),
        Value::ObjectPath(path) => Some(path.as_str().to_string()),
        _ => None,
    }
}

fn object_path(path: &str) -> Result<OwnedObjectPath, DesktopError> {
    OwnedObjectPath::try_from(path.to_string())
        .map_err(|err| DesktopError::Input(format!("session handle: {err}")))
}

fn evdev_button(button: Button) -> i32 {
    match button {
        Button::Left => 0x110,
        Button::Right => 0x111,
        Button::Middle => 0x112,
    }
}

fn unavailable(err: impl std::fmt::Display) -> DesktopError {
    DesktopError::Unavailable(err.to_string())
}

fn input_err(err: impl std::fmt::Display) -> DesktopError {
    DesktopError::Input(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bus_name_becomes_a_path_token() {
        assert_eq!(bus_name_to_path_token(":1.42"), "1_42");
    }

    #[test]
    fn reads_a_file_uri_and_deletes_it() {
        let path =
            std::env::temp_dir().join(format!("hypermesh-visor-{}.png", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"\x89PNG-test").unwrap();
        let uri = format!("file://{}", path.display());
        let bytes = read_local_uri(&uri).unwrap();
        assert_eq!(bytes, b"\x89PNG-test");
        assert!(!path.exists());
    }

    #[test]
    fn rejects_remote_uris() {
        assert!(file_uri_path("https://example.invalid/shot.png").is_err());
        assert!(file_uri_path("file://evil.example/tmp/shot.png").is_err());
    }
}
