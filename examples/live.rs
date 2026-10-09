// cargo run --example live -- example.com   (key in SDA_API_KEY)
fn main() {
    let key = std::env::var("SDA_API_KEY").expect("set SDA_API_KEY");
    let domain = std::env::args().nth(1).unwrap_or_else(|| "example.com".into());
    let c = selldatatoai::Client::new(&key);
    match c.score(&domain) {
        Ok(s) => println!("{} {} {} {:?}", s.domain, s.data_asset_score, s.grade, s.history.years_online),
        Err(e) => eprintln!("{}", e),
    }
}
