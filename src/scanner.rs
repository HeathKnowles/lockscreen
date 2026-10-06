use std::collections::HashMap;
use std::sync::mpsc::Sender;

use zbus::MatchRule;
use zbus::blocking::{Connection, MessageIterator};
use zbus::message::Type;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};

pub type Props = HashMap<String, OwnedValue>;
pub type ManagedObjects = HashMap<String, HashMap<String, Props>>;

#[derive(Debug)]
pub enum Event {
    Props { path: String, props: Props },
    Removed { path: String },
}

pub fn get_managed_objects(conn: &Connection) -> Result<ManagedObjects, String> {
    let reply = conn
        .call_method(
            Some("org.bluez"),
            "/",
            Some("org.freedesktop.DBus.ObjectManager"),
            "GetManagedObjects",
            &(),
        )
        .map_err(|e| e.to_string())?;
    let raw: HashMap<OwnedObjectPath, HashMap<String, Props>> =
        reply.body().deserialize().map_err(|e| e.to_string())?;
    Ok(raw
        .into_iter()
        .map(|(path, ifaces)| (path.to_string(), ifaces))
        .collect())
}

pub fn find_adapter(conn: &Connection) -> Result<String, String> {
    let objects = get_managed_objects(conn)?;
    for path in objects.keys() {
        if let Some(rest) = path.strip_prefix("/org/bluez/")
            && !rest.contains('/')
        {
            return Ok(rest.to_string());
        }
    }
    Err("no bluetooth adapter found on org.bluez".to_string())
}

pub fn start_discovery(conn: &Connection, adapter: &str) -> Result<(), String> {
    let path = format!("/org/bluez/{adapter}");
    let mut filter: HashMap<&str, Value> = HashMap::new();
    filter.insert("Transport", Value::from("le"));
    filter.insert("DuplicateData", Value::from(true));

    conn.call_method(
        Some("org.bluez"),
        path.as_str(),
        Some("org.bluez.Adapter1"),
        "SetDiscoveryFilter",
        &filter,
    )
    .map_err(|e| format!("SetDiscoveryFilter failed: {e}"))?;

    conn.call_method(
        Some("org.bluez"),
        path.as_str(),
        Some("org.bluez.Adapter1"),
        "StartDiscovery",
        &(),
    )
    .map_err(|e| format!("StartDiscovery failed: {e}"))?;
    Ok(())
}

pub fn stop_discovery(conn: &Connection, adapter: &str) {
    let path = format!("/org/bluez/{adapter}");
    let _ = conn.call_method(
        Some("org.bluez"),
        path.as_str(),
        Some("org.bluez.Adapter1"),
        "StopDiscovery",
        &(),
    );
}

/// Register the signal match rule. Must happen before discovery starts so that no
/// `InterfacesAdded` event is missed.
pub fn subscribe(conn: &Connection) -> Result<MessageIterator, String> {
    let rule = MatchRule::builder()
        .msg_type(Type::Signal)
        .sender("org.bluez")
        .map_err(|e| e.to_string())?
        .build();
    MessageIterator::for_match_rule(rule, conn, Some(256)).map_err(|e| e.to_string())
}

pub fn consume(mut iter: MessageIterator, tx: Sender<Event>) -> Result<(), String> {
    for msg in &mut iter {
        let msg = msg.map_err(|e| e.to_string())?;
        dispatch(&msg, &tx);
    }
    Ok(())
}

fn dispatch(msg: &zbus::Message, tx: &Sender<Event>) {
    let header = msg.header();
    let Some(iface) = header.interface() else {
        return;
    };
    let Some(member) = header.member() else {
        return;
    };
    let path = header.path().map(|p| p.to_string()).unwrap_or_default();

    match (iface.as_str(), member.as_str()) {
        ("org.freedesktop.DBus.ObjectManager", "InterfacesAdded") => {
            if let Ok((obj, mut ifaces)) = msg
                .body()
                .deserialize::<(OwnedObjectPath, HashMap<String, Props>)>()
                && let Some(props) = ifaces.remove("org.bluez.Device1")
            {
                let _ = tx.send(Event::Props {
                    path: obj.to_string(),
                    props,
                });
            }
        }
        ("org.freedesktop.DBus.ObjectManager", "InterfacesRemoved") => {
            if let Ok((obj, ifaces)) = msg.body().deserialize::<(OwnedObjectPath, Vec<String>)>()
                && ifaces.iter().any(|i| i == "org.bluez.Device1")
            {
                let _ = tx.send(Event::Removed {
                    path: obj.to_string(),
                });
            }
        }
        ("org.freedesktop.DBus.Properties", "PropertiesChanged") => {
            if let Ok((changed_iface, changed, _invalidated)) =
                msg.body().deserialize::<(String, Props, Vec<String>)>()
                && changed_iface == "org.bluez.Device1"
            {
                let _ = tx.send(Event::Props {
                    path,
                    props: changed,
                });
            }
        }
        _ => {}
    }
}
