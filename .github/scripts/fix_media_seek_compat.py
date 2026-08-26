from pathlib import Path

app = Path('src/app.rs')
text = app.read_text()
old = '''        let request = self.media_players.get(&group).and_then(|player| {
            let length = player.length_micros.filter(|length| *length > 0)?;
            let track_id = player.track_id.clone()?;
            if !player.can_seek {
                return None;
            }
            Some((
                player.bus_name.clone(),
                track_id,
                media_position_from_fraction(length, fraction),
            ))
        });
'''
new = '''        let request = self.media_players.get(&group).and_then(|player| {
            let length = player.length_micros.filter(|length| *length > 0)?;
            if !player.can_seek {
                return None;
            }
            Some((
                player.bus_name.clone(),
                player.track_id.clone().unwrap_or_default(),
                media_position_from_fraction(length, fraction),
            ))
        });
'''
if old not in text:
    raise SystemExit('app seek request block not found')
text = text.replace(old, new, 1)
old = '            let seekable = player.can_seek && player.track_id.is_some();\n'
new = '            let seekable = player.can_seek;\n'
if old not in text:
    raise SystemExit('seekable condition not found')
text = text.replace(old, new, 1)
app.write_text(text)

media = Path('src/bin/tihulu-mediad/main.rs')
text = media.read_text()
text = text.replace('    zvariant::{ObjectPath, OwnedObjectPath, OwnedValue},\n', '    zvariant::{OwnedObjectPath, OwnedValue},\n', 1)
old = '''async fn seek_player(
    connection: &Connection,
    bus_name: &str,
    track_id: &str,
    position_micros: i64,
) -> Result<(), String> {
    if !bus_name.starts_with("org.mpris.MediaPlayer2.") {
        return Err("invalid MPRIS bus name".to_owned());
    }
    if position_micros < 0 {
        return Err("invalid negative media seek position".to_owned());
    }
    let path = ObjectPath::try_from(track_id)
        .map_err(|error| format!("invalid MPRIS track id: {error}"))?;
    let player = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_PLAYER)
        .await
        .map_err(|error| format!("MPRIS player proxy failed: {error}"))?;
    let can_seek: bool = player.get_property("CanSeek").await.unwrap_or(false);
    if !can_seek {
        return Err("MPRIS player does not support seek".to_owned());
    }
    player
        .call_method("SetPosition", &(path, position_micros))
        .await
        .map_err(|error| format!("MPRIS SetPosition failed: {error}"))?;
    Ok(())
}
'''
new = '''async fn seek_player(
    connection: &Connection,
    bus_name: &str,
    _track_id: &str,
    position_micros: i64,
) -> Result<(), String> {
    if !bus_name.starts_with("org.mpris.MediaPlayer2.") {
        return Err("invalid MPRIS bus name".to_owned());
    }
    if position_micros < 0 {
        return Err("invalid negative media seek position".to_owned());
    }
    let player = Proxy::new(connection, bus_name, MPRIS_PATH, MPRIS_PLAYER)
        .await
        .map_err(|error| format!("MPRIS player proxy failed: {error}"))?;
    let can_seek: bool = player.get_property("CanSeek").await.unwrap_or(false);
    if !can_seek {
        return Err("MPRIS player does not support seek".to_owned());
    }

    let metadata: HashMap<String, OwnedValue> =
        player.get_property("Metadata").await.unwrap_or_default();
    if let Some(path) = metadata
        .get("mpris:trackid")
        .and_then(|value| OwnedObjectPath::try_from(value.clone()).ok())
        && player
            .call_method("SetPosition", &(path, position_micros))
            .await
            .is_ok()
    {
        return Ok(());
    }

    let current = player
        .get_property::<OwnedValue>("Position")
        .await
        .ok()
        .as_ref()
        .and_then(integer_micros)
        .unwrap_or(0)
        .max(0);
    let offset = position_micros.saturating_sub(current);
    player
        .call_method("Seek", &(offset,))
        .await
        .map_err(|error| format!("MPRIS seek failed: {error}"))?;
    Ok(())
}
'''
if old not in text:
    raise SystemExit('daemon seek_player block not found')
text = text.replace(old, new, 1)
media.write_text(text)
