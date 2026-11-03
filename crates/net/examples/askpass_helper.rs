//! Помощник `SSH_ASKPASS` для стендового теста `term_ssh` (как `urfin.exe --askpass`):
//! запрос — в pipe `URFIN_ASKPASS`, ответ — в stdout.

fn main() {
    let prompt = std::env::args().nth(1).unwrap_or_default();
    std::process::exit(urfin_net::bridge::askpass::run(&prompt));
}
