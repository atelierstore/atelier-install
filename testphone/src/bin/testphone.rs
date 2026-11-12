//! The stand-in iPhone, from the command line, for tests that run the real
//! programs:
//!
//!   testphone pair <address> <port>     dials the program, reads
//!       the code from its standard input once the host has made it (a line
//!       saying `code?` asks for it), pairs, and prints the host it stored
//!       as `identifier <id>` and `public_key <hex>`
//!   testphone drop <address> <port>     connects, handshakes and hangs up,
//!       as a connection that ends before pairing starts
use std::io::BufRead;
use std::process::ExitCode;

use atelier_testphone::Phone;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = match args.as_slice() {
        ["pair", address, port] => run(pair(address, port)),
        ["drop", address, port] => run(drop_early(address, port)),
        _ => Err("usage: testphone pair|drop <address> <port>".into()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(problem) => {
            eprintln!("testphone: {problem}");
            ExitCode::FAILURE
        }
    }
}

fn run(work: impl Future<Output = Result<(), String>>) -> Result<(), String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?
        .block_on(work)
}

fn port(text: &str) -> Result<u16, String> {
    text.parse().map_err(|_| format!("`{text}` isn't a port"))
}

async fn pair(address: &str, port_text: &str) -> Result<(), String> {
    let mut phone = Phone::connect(address, port(port_text)?).await?;
    phone.handshake().await?;
    let (salt, b_pub) = phone.start_pair_setup().await?;
    println!("code?");
    let mut code = String::new();
    std::io::stdin()
        .lock()
        .read_line(&mut code)
        .map_err(|e| e.to_string())?;
    let code: String = code.chars().filter(char::is_ascii_digit).collect();
    let host = phone.finish_pair_setup(&code, &salt, &b_pub).await?;
    println!("identifier {}", host.identifier);
    println!("public_key {}", hex(&host.public_key));
    println!("phone_alt_irk {}", hex(&host.phone_alt_irk));
    Ok(())
}

async fn drop_early(address: &str, port_text: &str) -> Result<(), String> {
    let mut phone = Phone::connect(address, port(port_text)?).await?;
    phone.handshake().await
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
