//! reel: a swaybg-style Wayland wallpaper daemon with a directory slideshow.

mod app;
mod cli;
mod decode;
mod playlist;
mod render;

use std::process::exit;

use smithay_client_toolkit::reexports::{
    calloop::{
        EventLoop,
        timer::{TimeoutAction, Timer},
    },
    calloop_wayland_source::WaylandSource,
    client::{Connection, globals::registry_queue_init},
};

use app::Reel;

fn main() {
    app::tune_malloc();
    let configs = cli::parse_args(std::env::args().skip(1));

    let conn = Connection::connect_to_env().unwrap_or_else(|e| {
        eprintln!("reel: unable to connect to the compositor: {e}");
        exit(1)
    });
    let (globals, queue) = registry_queue_init(&conn).expect("wl_registry");
    let qh = queue.handle();

    let mut event_loop: EventLoop<Reel> = EventLoop::try_new().expect("event loop");
    for (i, c) in configs.iter().enumerate() {
        if let Some(interval) = c.interval {
            event_loop
                .handle()
                .insert_source(Timer::from_duration(interval), move |_, _, reel| {
                    reel.advance(i);
                    TimeoutAction::ToDuration(interval)
                })
                .expect("timer");
        }
    }

    let mut reel = Reel::new(&globals, &qh, configs);
    WaylandSource::new(conn, queue)
        .insert(event_loop.handle())
        .expect("wayland source");
    // Draw once per batch of Wayland events, like swaybg's main loop.
    event_loop
        .run(None, &mut reel, Reel::flush)
        .expect("event loop");
}
