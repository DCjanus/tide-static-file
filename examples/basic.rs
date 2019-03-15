use tide_static_file::StaticFiles;

fn main() {
    let mut app = tide::App::new(());
    app.at("/static/*").get(StaticFiles::new("./").unwrap());

    let config = tide::configuration::ConfigurationBuilder::default()
        .address("127.0.0.1")
        .port(8000)
        .finalize();

    app.config(config);
    app.serve()
}
