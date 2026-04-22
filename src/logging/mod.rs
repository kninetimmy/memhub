pub fn init() {
    let env = env_logger::Env::default().filter_or("MEMHUB_LOG", "info");
    let _ = env_logger::Builder::from_env(env)
        .format_timestamp_secs()
        .try_init();
}
