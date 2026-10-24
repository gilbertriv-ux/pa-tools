//! Control-thread hardware setup. MIDI packets go directly to the audio queue after connection.
use crate::SoundApp;
use serde_json::{Value, json};
use soundcraft_model::{TrackId, TrackKind};
use soundcraft_playback::midi_devices::{MidiInputConnection, MidiInputPort, MidiSystem};

#[derive(Default)]
pub(crate) struct HardwareState {
    midi_open: bool,
    routing_open: bool,
    system: soundcraft_playback::midi_hardware::NativeMidiSystem,
    ports: Vec<MidiInputPort>,
    port: usize,
    target: Option<TrackId>,
    channel: u8,
    connection: Option<Box<dyn MidiInputConnection>>,
    status: String,
    routing: Option<soundcraft_playback::AudioConfig>,
}

pub(crate) fn is_live(app: &SoundApp) -> bool {
    app.hardware.connection.is_some()
}

pub(crate) fn disconnect(app: &mut SoundApp) {
    if let Some(player) = &app.player {
        player.detach_midi_input();
    }
    if app.hardware.connection.take().is_some() {
        app.hardware.status = "MIDI Thru disconnected. Reconnect after changing the audio engine or session.".into();
    }
}

fn refresh(state: &mut HardwareState) -> Result<Value, String> {
    state.ports = state.system.inputs().map_err(|e| e.to_string())?;
    state.port = 0;
    Ok(json!(state.ports.iter().map(|p| json!({"id": p.id.as_str(), "name": p.name})).collect::<Vec<_>>()))
}

fn connect(app: &mut SoundApp, state: &mut HardwareState) -> Result<Value, String> {
    let target = state.target.ok_or("Choose an instrument track")?;
    let track = app.engine.session().track(target).ok_or("Instrument track is no longer available")?;
    if track.kind != TrackKind::Instrument {
        return Err("MIDI Thru needs an instrument track".into());
    }
    if track.instrument.is_none() {
        app.engine.execute("mix.instrument", &json!({"track": target.0, "plugin": "subtractive_synth"})).map_err(|e| e.to_string())?;
    }
    let player = app.player.as_ref().filter(|p| !p.silent).ok_or("Open a working audio output first")?;
    let port = state.ports.get(state.port).ok_or("Choose a MIDI input port")?.clone();
    let mut connection = state.system.open_input(&port).map_err(|e| e.to_string())?;
    let receiver = connection.take_receiver().ok_or("MIDI backend cannot hand its queue to audio")?;
    player.update_session(app.engine.session_arc());
    player.attach_midi_input(receiver, target, state.channel.checked_sub(1)).map_err(|e| e.to_string())?;
    state.connection = Some(connection);
    state.status = format!("Monitoring {}. Play your keyboard; recording is not required.", port.name);
    Ok(json!({"connected": true, "track": target.0, "port": port.id.as_str()}))
}

pub(crate) fn command(app: &mut SoundApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if !matches!(
        id,
        "window.midi_thru" | "window.channel_routing" | "midi.thru.inputs" | "midi.thru.connect" | "midi.thru.disconnect" | "midi.thru.status"
    ) {
        return None;
    }
    let mut state = std::mem::take(&mut app.hardware);
    let result = match id {
        "window.midi_thru" => {
            state.midi_open = params.get("value").and_then(Value::as_bool).unwrap_or(!state.midi_open);
            Ok(json!({"value": state.midi_open}))
        }
        "window.channel_routing" => {
            state.routing_open = params.get("value").and_then(Value::as_bool).unwrap_or(!state.routing_open);
            state.routing = Some(app.ui.audio.clone());
            Ok(json!({"value": state.routing_open}))
        }
        "midi.thru.inputs" => refresh(&mut state),
        "midi.thru.connect" => {
            if let Some(track) = params.get("track").and_then(Value::as_u64) {
                state.target = Some(TrackId(track));
            }
            if let Some(port) = params.get("port").and_then(Value::as_str) {
                state.port = state.ports.iter().position(|p| p.id.as_str() == port).unwrap_or(usize::MAX);
            }
            if let Some(channel) = params.get("channel").and_then(Value::as_u64) {
                if channel > 16 {
                    app.hardware = state;
                    return Some(Err("MIDI channel must be 0 (all) or 1..16".into()));
                }
                state.channel = channel as u8;
            }
            connect(app, &mut state)
        }
        "midi.thru.disconnect" => {
            if let Some(player) = &app.player {
                player.detach_midi_input();
            }
            state.connection = None;
            state.status = "MIDI Thru disconnected".into();
            Ok(json!({"connected": false}))
        }
        _ => {
            Ok(json!({"connected": state.connection.is_some(), "track": state.target.map(|t| t.0), "channel": state.channel, "status": state.status}))
        }
    };
    if let Err(error) = &result {
        state.status = error.clone();
        app.ui.status = error.clone();
    }
    app.hardware = state;
    Some(result)
}

pub(crate) fn show(app: &mut SoundApp, ctx: &egui::Context) {
    let mut state = std::mem::take(&mut app.hardware);
    let mut midi_open = state.midi_open;
    egui::Window::new("MIDI Thru").open(&mut midi_open).default_width(410.0).show(ctx, |ui| {
        ui.label("Play a MIDI keyboard through an instrument track, with the transport stopped or playing.");
        if ui.button("Refresh MIDI inputs").clicked() {
            match refresh(&mut state) {
                Ok(_) => {
                    state.status = if state.ports.is_empty() { "No MIDI inputs found".into() } else { "Choose an input and instrument track".into() }
                }
                Err(e) => state.status = e,
            }
        }
        let connected = state.connection.is_some();
        ui.add_enabled_ui(!connected, |ui| {
            egui::ComboBox::from_id_salt("midi_thru_port")
                .selected_text(state.ports.get(state.port).map_or("Select MIDI input", |p| p.name.as_str()))
                .show_ui(ui, |ui| {
                    for (index, port) in state.ports.iter().enumerate() {
                        ui.selectable_value(&mut state.port, index, &port.name);
                    }
                });
            let tracks: Vec<_> = app
                .engine
                .session()
                .tracks
                .iter()
                .filter(|t| t.kind == TrackKind::Instrument && !t.inactive)
                .map(|t| (t.id, t.name.clone()))
                .collect();
            let name = tracks.iter().find(|(id, _)| Some(*id) == state.target).map_or("Select instrument track", |(_, name)| name.as_str());
            egui::ComboBox::from_id_salt("midi_thru_target").selected_text(name).show_ui(ui, |ui| {
                for (id, name) in &tracks {
                    ui.selectable_value(&mut state.target, Some(*id), name);
                }
            });
            if ui.button("Create instrument track").clicked() {
                match app.run("track.new", json!({"kind": "instrument", "format": "stereo", "name": "Live Keyboard"})) {
                    Ok(_) => state.target = app.engine.session().tracks.last().map(|t| t.id),
                    Err(e) => state.status = e,
                }
            }
            egui::ComboBox::from_id_salt("midi_thru_channel")
                .selected_text(if state.channel == 0 { "All MIDI channels".into() } else { format!("MIDI channel {}", state.channel) })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut state.channel, 0, "All MIDI channels");
                    for channel in 1..=16 {
                        ui.selectable_value(&mut state.channel, channel, format!("Channel {channel}"));
                    }
                });
        });
        if connected {
            if ui.button("Disconnect / All notes off").clicked() {
                if let Some(player) = &app.player {
                    player.detach_midi_input();
                }
                state.connection = None;
                state.status = "MIDI Thru disconnected".into();
            }
        } else if ui.button("Enable MIDI Thru").clicked() {
            if let Err(e) = connect(app, &mut state) {
                state.status = e;
            }
        }
        ui.label(&state.status);
    });
    state.midi_open = midi_open;
    let mut routing_open = state.routing_open;
    let mut apply = None;
    egui::Window::new("Channel Routing").open(&mut routing_open).default_width(400.0).show(ctx, |ui| {
        ui.label("Route session main channels to physical interface outputs. A source may feed several outputs.");
        if let Some(player) = &app.player {
            ui.label(format!("{} · {} open channels", player.device_name, player.device_channels));
        }
        let physical = app.player.as_ref().map_or(2, |p| usize::from(p.device_channels).max(1));
        let main = app.engine.session().main_format().channels();
        let cfg = state.routing.get_or_insert_with(|| app.ui.audio.clone());
        let mut explicit = !cfg.output_channels.is_empty();
        if ui.checkbox(&mut explicit, "Use explicit output routing").changed() {
            cfg.output_channels = if explicit { (0..physical).map(|i| (i < main).then_some(i)).collect() } else { Vec::new() };
        }
        if explicit {
            let mut outputs = cfg.output_channels.len();
            ui.horizontal(|ui| {
                ui.label("Physical outputs to open");
                if ui.add(egui::DragValue::new(&mut outputs).range(1..=64)).changed() {
                    cfg.output_channels.resize(outputs, None);
                }
            });
            egui::ScrollArea::vertical().max_height(350.0).show(ui, |ui| {
                for (index, source) in cfg.output_channels.iter_mut().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("Output {}", index + 1));
                        egui::ComboBox::from_id_salt(("physical_output", index))
                            .selected_text(source.map_or("Muted".into(), |i| format!("Main {}", i + 1)))
                            .show_ui(ui, |ui| {
                                ui.selectable_value(source, None, "Muted");
                                for input in 0..main {
                                    ui.selectable_value(source, Some(input), format!("Main {}", input + 1));
                                }
                            });
                    });
                }
            });
        }
        ui.label("Choose the interface and buffer in Setup > Playback Engine first.");
        if ui.add_enabled(!app.engine.transport.recording, egui::Button::new("Apply routing")).clicked() {
            apply = Some(cfg.clone());
        }
    });
    state.routing_open = routing_open;
    app.hardware = state;
    if let Some(cfg) = apply {
        if let Err(e) = app.apply_audio_config(cfg) {
            app.ui.status = e;
        } else {
            app.ui.status = match &app.player {
                Some(player) if !player.silent => format!("Output routing applied: {} channels on {}", player.device_channels, player.device_name),
                _ => "Output routing could not open an audio device. Check the interface and channel count.".into(),
            };
        }
    }
}
