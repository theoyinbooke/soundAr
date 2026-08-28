fn main() {
    let mut arguments = std::env::args_os();
    let _executable = arguments.next();
    let remaining = arguments.collect::<Vec<_>>();
    if remaining.first().and_then(|value| value.to_str()) == Some("agent") {
        std::process::exit(soundar_desktop_lib::run_agent_cli(
            remaining.into_iter().skip(1).collect(),
        ));
    }
    soundar_desktop_lib::run();
}
