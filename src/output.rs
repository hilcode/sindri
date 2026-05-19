use std::io;

pub trait Output: io::Write {
    fn info(&mut self, message: &str);
}

pub struct TerminalOutput;

impl io::Write for TerminalOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        io::stdout().write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        io::stdout().flush()
    }
}

impl Output for TerminalOutput {
    fn info(&mut self, message: &str) {
        tracing::info!("{message}");
    }
}

#[cfg(test)]
pub struct CapturedOutput {
    pub stdout: Vec<u8>,
    pub log: Vec<String>,
}

#[cfg(test)]
impl CapturedOutput {
    pub fn new() -> Self {
        Self {
            stdout: Vec::new(),
            log: Vec::new(),
        }
    }

    pub fn stdout_str(&self) -> &str {
        std::str::from_utf8(&self.stdout).expect("stdout is valid UTF-8")
    }
}

#[cfg(test)]
impl io::Write for CapturedOutput {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.stdout.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
impl Output for CapturedOutput {
    fn info(&mut self, message: &str) {
        self.log.push(message.to_owned());
    }
}
