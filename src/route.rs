//! The anisette servers one sign-in goes through, one at a time, and when
//! it stops: the app's rules (`AnisetteRoute` in AppleIDKit), over the same
//! list, which the program takes from Atelier's worker or, failing that,
//! from the copy the app is built with.

use serde_json::Value;

/// The list the app is built with: a copy of Atelier's
/// `Packages/AtelierKit/Sources/AppleIDKit/anisette.json`.
const BUILT_IN: &str = include_str!("anisette.json");
/// The version of the list's format this program reads.
const VERSION: u64 = 1;
/// How many more servers a sign-in tries after Apple's first -80045.
const MORE_AFTER_REFUSAL: u32 = 2;

#[derive(Debug, Clone, PartialEq)]
pub struct Server {
    pub url: String,
    pub name: String,
    /// A Mac's, Windows' ADI or another; Apple can refuse one kind only.
    pub kind: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct List {
    /// The most servers one sign-in tries.
    pub tries: usize,
    /// What the servers' machines say they are, when the list says.
    pub client_info: Option<(String, String)>,
    pub servers: Vec<Server>,
}

impl List {
    /// Reads a list, or None when it's another version's or has no server.
    pub fn parse(json: &str) -> Option<List> {
        let value: Value = serde_json::from_str(json).ok()?;
        if value.get("version")?.as_u64()? != VERSION {
            return None;
        }
        let servers: Vec<Server> = value
            .get("servers")?
            .as_array()?
            .iter()
            .filter_map(|server| {
                let url = server
                    .get("url")?
                    .as_str()?
                    .trim_end_matches('/')
                    .to_string();
                if !url.starts_with("https://") {
                    return None;
                }
                let name = server
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or(&url)
                    .to_string();
                let kind = server
                    .get("kind")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                Some(Server { url, name, kind })
            })
            .collect();
        if servers.is_empty() {
            return None;
        }
        let client_info = value.get("clientInfo").and_then(|info| {
            Some((
                info.get("clientInfo")?.as_str()?.to_string(),
                info.get("userAgent")?.as_str()?.to_string(),
            ))
        });
        let tries = value.get("tries").and_then(Value::as_u64).unwrap_or(4) as usize;
        Some(List {
            tries,
            client_info,
            servers,
        })
    }

    pub fn built_in() -> List {
        List::parse(BUILT_IN).expect("the app's anisette list")
    }

    /// Only the user's own server.
    pub fn own(url: &str) -> List {
        List {
            tries: 1,
            client_info: None,
            servers: vec![Server {
                url: url.trim_end_matches('/').to_string(),
                name: url.to_string(),
                kind: None,
            }],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Failure {
    /// The server didn't answer, or Apple's sign-in failed through it.
    Server,
    /// Apple's "Try again" (-22421): the same server once more first.
    TryAgain,
    /// Apple's "This action cannot be completed at this time" (-80045):
    /// two more servers at most, of another kind.
    Refused,
    /// Nothing another server would change: a wrong password, a locked
    /// Apple ID, a cancelled code.
    Final,
}

/// What Apple's sign-in error `code` means for the route.
pub fn failure_of(code: i64) -> Failure {
    match code {
        -20101 | -20209 => Failure::Final,
        -22421 => Failure::TryAgain,
        -80045 => Failure::Refused,
        _ => Failure::Server,
    }
}

pub struct Route {
    servers: Vec<Server>,
    tries: usize,
    tried: usize,
    position: Option<usize>,
    tried_again: bool,
    refusals: u32,
    refused_kinds: Vec<String>,
}

impl Route {
    /// Through the list, starting with the server the last sign-in worked
    /// through, as Apple may trust its machine by then and not ask for a
    /// code.
    pub fn new(list: &List, last_worked: Option<&str>) -> Route {
        let mut servers = list.servers.clone();
        if let Some(index) =
            last_worked.and_then(|url| servers.iter().position(|server| server.url == url))
        {
            let server = servers.remove(index);
            servers.insert(0, server);
        }
        Route {
            tries: list.tries.clamp(1, servers.len()),
            servers,
            tried: 0,
            position: None,
            tried_again: false,
            refusals: 0,
            refused_kinds: Vec::new(),
        }
    }

    /// The first server to try.
    pub fn start(&mut self) -> Option<Server> {
        self.advance(0)
    }

    /// The server to try after `failure` through the current one: the same
    /// one again, the next one, or None to stop.
    pub fn next(&mut self, failure: Failure) -> Option<Server> {
        let position = self.position?;
        match failure {
            Failure::Final => None,
            Failure::TryAgain if !self.tried_again => {
                self.tried_again = true;
                Some(self.servers[position].clone())
            }
            Failure::TryAgain | Failure::Server => self.advance(position + 1),
            Failure::Refused => {
                self.refusals += 1;
                if self.refusals > MORE_AFTER_REFUSAL {
                    return None;
                }
                if let Some(kind) = &self.servers[position].kind {
                    self.refused_kinds.push(kind.clone());
                }
                self.advance(position + 1)
            }
        }
    }

    /// The first server from `index` on to try, skipping those of a kind
    /// Apple refused through, while fewer than `tries` were tried.
    fn advance(&mut self, index: usize) -> Option<Server> {
        if self.tried >= self.tries {
            return None;
        }
        let refused = &self.refused_kinds;
        let candidate = (index..self.servers.len()).find(|candidate| {
            self.servers[*candidate]
                .kind
                .as_ref()
                .is_none_or(|kind| !refused.contains(kind))
        })?;
        self.position = Some(candidate);
        self.tried_again = false;
        self.tried += 1;
        Some(self.servers[candidate].clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(kinds: &[Option<&str>], tries: usize) -> List {
        List {
            tries,
            client_info: None,
            servers: kinds
                .iter()
                .enumerate()
                .map(|(index, kind)| Server {
                    url: format!("https://s{index}.example"),
                    name: format!("s{index}"),
                    kind: kind.map(str::to_owned),
                })
                .collect(),
        }
    }

    fn url(server: Option<Server>) -> Option<String> {
        server.map(|server| server.url)
    }

    #[test]
    fn the_built_in_list_is_the_apps() {
        let list = List::built_in();
        assert_eq!(list.tries, 4);
        assert_eq!(list.servers[0].url, "https://mac-ani.jkcoxson.com");
        assert_eq!(list.servers[0].kind.as_deref(), Some("mac"));
        assert!(list.servers.len() > 10);
        let (client_info, user_agent) = list.client_info.unwrap();
        assert!(client_info.contains("com.apple.AuthKit"));
        assert!(user_agent.starts_with("akd/"));
    }

    #[test]
    fn a_list_of_another_version_or_without_servers_isnt_read() {
        assert!(List::parse(r#"{"version":2,"servers":[{"url":"https://a.example"}]}"#).is_none());
        assert!(List::parse(r#"{"version":1,"servers":[]}"#).is_none());
        assert!(List::parse(r#"{"version":1,"servers":[{"url":"http://a.example"}]}"#).is_none());
        assert!(List::parse("<html>").is_none());
        let read =
            List::parse(r#"{"version":1,"servers":[{"url":"https://a.example/"}]}"#).unwrap();
        assert_eq!(read.servers[0].url, "https://a.example");
        assert_eq!(read.tries, 4);
    }

    #[test]
    fn servers_in_order_up_to_the_lists_tries() {
        let mut route = Route::new(&list(&[None, None, None, None], 3), None);
        assert_eq!(url(route.start()).as_deref(), Some("https://s0.example"));
        assert_eq!(
            url(route.next(Failure::Server)).as_deref(),
            Some("https://s1.example")
        );
        assert_eq!(
            url(route.next(Failure::Server)).as_deref(),
            Some("https://s2.example")
        );
        assert_eq!(route.next(Failure::Server), None);
    }

    #[test]
    fn the_server_that_worked_last_goes_first() {
        let mut route = Route::new(&list(&[None, None, None], 3), Some("https://s2.example"));
        assert_eq!(url(route.start()).as_deref(), Some("https://s2.example"));
        assert_eq!(
            url(route.next(Failure::Server)).as_deref(),
            Some("https://s0.example")
        );
    }

    #[test]
    fn try_again_is_the_same_server_once() {
        let mut route = Route::new(&list(&[None, None], 2), None);
        route.start();
        assert_eq!(
            url(route.next(Failure::TryAgain)).as_deref(),
            Some("https://s0.example")
        );
        assert_eq!(
            url(route.next(Failure::TryAgain)).as_deref(),
            Some("https://s1.example")
        );
    }

    #[test]
    fn a_refusal_moves_to_another_kind_twice_at_most() {
        let kinds = [
            Some("mac"),
            Some("mac"),
            Some("other"),
            Some("windows"),
            None,
        ];
        let mut route = Route::new(&list(&kinds, 5), None);
        route.start();
        assert_eq!(
            url(route.next(Failure::Refused)).as_deref(),
            Some("https://s2.example")
        );
        assert_eq!(
            url(route.next(Failure::Refused)).as_deref(),
            Some("https://s3.example")
        );
        assert_eq!(route.next(Failure::Refused), None);
    }

    #[test]
    fn a_final_failure_stops() {
        let mut route = Route::new(&list(&[None, None], 2), None);
        route.start();
        assert_eq!(route.next(failure_of(-20101)), None);
        assert_eq!(failure_of(-20209), Failure::Final);
        assert_eq!(failure_of(-22421), Failure::TryAgain);
        assert_eq!(failure_of(-80045), Failure::Refused);
        assert_eq!(failure_of(-1), Failure::Server);
    }
}
