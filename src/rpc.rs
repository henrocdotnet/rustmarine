//! LSP4J's Content-Length framed, bidirectional JSON-RPC transport.
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;
use std::time::Instant;

const MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;

pub(crate) fn read_message(reader: &mut impl BufRead) -> Result<Value> {
    let mut length = None;
    let mut total_header = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            bail!("Sonar backend closed its output stream");
        }
        total_header += line.len();
        if total_header > 8192 {
            bail!("Sonar backend sent oversized RPC headers");
        }
        if line == "\r\n" || line == "\n" {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            if name.eq_ignore_ascii_case("Content-Length") {
                if length.is_some() {
                    bail!("Sonar backend sent duplicate Content-Length headers");
                }
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .context("Invalid RPC Content-Length")?,
                );
            }
        }
    }
    let length = length.context("RPC frame is missing Content-Length")?;
    if length > MAX_MESSAGE_BYTES {
        bail!("Sonar backend RPC message exceeds 64 MiB");
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body)?;
    serde_json::from_slice(&body).context("Invalid JSON from Sonar backend")
}

fn write_message(writer: &mut impl Write, value: &Value) -> Result<()> {
    let body = serde_json::to_vec(value)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()?;
    Ok(())
}

pub struct Rpc {
    child: Child,
    input: Option<ChildStdin>,
    incoming: mpsc::Receiver<Result<Value>>,
    stderr: Arc<Mutex<String>>,
    next_id: u64,
}

impl Rpc {
    pub fn spawn(command: &mut Command, verbose: bool) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .context("Cannot launch Sonar backend; configure a Java 21+ executable with --java")?;
        let input = child.stdin.take().context("Missing backend stdin")?;
        let output = child.stdout.take().context("Missing backend stdout")?;
        let error = child.stderr.take().context("Missing backend stderr")?;
        let (send, incoming) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(output);
            loop {
                let message = read_message(&mut reader);
                let failed = message.is_err();
                if send.send(message).is_err() || failed {
                    break;
                }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let captured = Arc::clone(&stderr);
        thread::spawn(move || {
            for line in BufReader::new(error).lines() {
                let Ok(line) = line else { break };
                if verbose {
                    eprintln!("[sonar] {line}");
                }
                if let Ok(mut captured) = captured.lock() {
                    captured.push_str(&line);
                    captured.push('\n');
                    if captured.len() > 16384 {
                        let mut cut = captured.len() - 16384;
                        while !captured.is_char_boundary(cut) {
                            cut += 1;
                        }
                        captured.drain(..cut);
                    }
                }
            }
        });
        Ok(Self {
            child,
            input: Some(input),
            incoming,
            stderr,
            next_id: 1,
        })
    }

    fn send(&mut self, value: Value) -> Result<()> {
        write_message(self.input.as_mut().context("Backend stdin closed")?, &value)
    }

    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(json!({"jsonrpc":"2.0","method":method,"params":params}))
    }

    pub fn request<F>(
        &mut self,
        method: &str,
        params: Value,
        deadline: Instant,
        handler: &mut F,
    ) -> Result<Value>
    where
        F: FnMut(&str, &Value) -> Result<Value>,
    {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc":"2.0","id":id,"method":method,"params":params}))?;
        loop {
            let message = self.receive(deadline)?;
            if message.get("id") == Some(&json!(id)) && message.get("method").is_none() {
                if let Some(error) = message.get("error") {
                    bail!("Sonar backend {method} failed: {error}");
                }
                return message
                    .get("result")
                    .cloned()
                    .context("RPC response missing result");
            }
            self.dispatch(message, handler)?;
        }
    }

    pub fn pump<F>(&mut self, deadline: Instant, handler: &mut F) -> Result<()>
    where
        F: FnMut(&str, &Value) -> Result<Value>,
    {
        let message = self.receive(deadline)?;
        self.dispatch(message, handler)
    }

    fn receive(&mut self, deadline: Instant) -> Result<Value> {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("Sonar backend timed out")?;
        match self.incoming.recv_timeout(remaining) {
            Ok(Ok(message)) => Ok(message),
            Ok(Err(error)) => Err(anyhow!("{error}\n{}", self.stderr_tail())),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                bail!("Sonar backend timed out\n{}", self.stderr_tail())
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                bail!("Sonar backend disconnected\n{}", self.stderr_tail())
            }
        }
    }

    fn stderr_tail(&self) -> String {
        self.stderr.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn dispatch<F>(&mut self, message: Value, handler: &mut F) -> Result<()>
    where
        F: FnMut(&str, &Value) -> Result<Value>,
    {
        let Some(method) = message.get("method").and_then(Value::as_str) else {
            bail!("Unexpected Sonar backend RPC response: {message}");
        };
        let result = handler(method, message.get("params").unwrap_or(&Value::Null));
        if let Some(id) = message.get("id") {
            let response = match result {
                Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
                Err(error) => {
                    json!({"jsonrpc":"2.0","id":id,"error":{"code":-32601,"message":error.to_string()}})
                }
            };
            self.send(response)?;
        } else {
            result?;
        }
        Ok(())
    }
}

impl Drop for Rpc {
    fn drop(&mut self) {
        self.input.take();
        if !matches!(self.child.try_wait(), Ok(Some(_))) {
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn frames_use_utf8_byte_length_and_support_consecutive_messages() {
        let original = json!({"method":"log","params":{"message":"héllo 🦀"}});
        let mut encoded = Vec::new();
        write_message(&mut encoded, &original).unwrap();
        write_message(&mut encoded, &json!({"id":1,"result":null})).unwrap();
        let mut input = Cursor::new(encoded);
        assert_eq!(read_message(&mut input).unwrap(), original);
        assert_eq!(
            read_message(&mut input).unwrap(),
            json!({"id":1,"result":null})
        );
    }

    #[test]
    fn malformed_and_oversized_frames_fail() {
        for input in [
            "Content-Length: 999999999\r\n\r\n",
            "Content-Type: x\r\n\r\n",
            "Content-Length: 1\r\nContent-Length: 1\r\n\r\n{}",
            "Content-Length: 8\r\n\r\n{}",
        ] {
            assert!(read_message(&mut Cursor::new(input)).is_err());
        }
    }

    #[test]
    #[cfg(unix)]
    fn backend_exit_is_an_operational_error() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exit 7"]);
        let mut rpc = Rpc::spawn(&mut command, false).unwrap();
        let error = rpc
            .pump(
                Instant::now() + std::time::Duration::from_secs(2),
                &mut |_, _| Ok(Value::Null),
            )
            .unwrap_err();
        assert!(error.to_string().contains("closed its output stream"));
    }

    #[test]
    #[cfg(unix)]
    fn unresponsive_backend_respects_deadline() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let mut rpc = Rpc::spawn(&mut command, false).unwrap();
        let error = rpc
            .request(
                "initialize",
                Value::Null,
                Instant::now() + std::time::Duration::from_millis(20),
                &mut |_, _| Ok(Value::Null),
            )
            .unwrap_err();
        assert!(error.to_string().contains("timed out"));
        // Drop kills and reaps the sleeping child instead of waiting thirty seconds.
        drop(rpc);
    }
}
