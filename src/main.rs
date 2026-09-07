static POSSIBLE_BACKENDS: &[&str] = &[
    #[cfg(feature = "winit")]
    "--winit : Run nested as a X11 or Wayland client using winit (for development).",
    #[cfg(feature = "udev")]
    "--tty-udev : Run standalone on a tty using DRM/KMS + libinput (requires root if without logind/seatd).",
];

#[allow(clippy::uninlined_format_args)]
fn main() {
    if let Ok(env_filter) = tracing_subscriber::EnvFilter::try_from_default_env() {
        tracing_subscriber::fmt()
            .compact()
            .with_env_filter(env_filter)
            .init();
    } else {
        tracing_subscriber::fmt().compact().init();
    }

    profiling::register_thread!("Main Thread");

    let arg = ::std::env::args().nth(1);
    match arg.as_ref().map(|s| &s[..]) {
        #[cfg(feature = "winit")]
        Some("--winit") => {
            tracing::info!("Starting ironland-compositor with winit backend");
            ironland_compositor::winit::run_winit();
        }
        #[cfg(feature = "udev")]
        Some("--tty-udev") => {
            tracing::info!("Starting ironland-compositor on a tty using udev");
            ironland_compositor::udev::run_udev();
        }
        Some(other) => {
            tracing::error!("Unknown backend: {}", other);
        }
        None => {
            #[allow(clippy::disallowed_macros)]
            {
                println!("USAGE: ironland-compositor --backend");
                println!();
                println!("Possible backends are:");
                for b in POSSIBLE_BACKENDS {
                    println!("\t{b}");
                }
            }
        }
    }
}
