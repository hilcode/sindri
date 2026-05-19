use crate::executor::TaskOutcome;
use crate::runtime::FileSystem;
use crate::types::AbsoluteDirectory;
use crate::types::AbsoluteFile;
use crate::types::BuildStart;
use crate::types::Fiber;
use crate::types::RelativeFile;
use serde::Serialize;
use serde::Serializer;
use std::io::Result as IoResult;
use std::time::Duration;

fn serialize_duration_as_micros<S: Serializer>(duration: &Duration, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_u64(duration.as_micros() as u64)
}

#[derive(Serialize)]
struct TraceEvent {
    name: String,
    ph: &'static str,
    #[serde(serialize_with = "serialize_duration_as_micros")]
    ts: Duration,
    #[serde(serialize_with = "serialize_duration_as_micros")]
    dur: Duration,
    pid: u64,
    tid: Fiber,
    args: TraceArgs,
}

#[derive(Serialize)]
struct TraceArgs {
    cache: &'static str,
}

#[derive(Serialize)]
struct TraceFile {
    #[serde(rename = "traceEvents")]
    trace_events: Vec<TraceEvent>,
}

pub struct Telemetry;

impl Telemetry {
    pub fn write(
        outcomes: &[TaskOutcome],
        build_directory: &AbsoluteDirectory,
        build_start: BuildStart,
        file_system: &impl FileSystem,
    ) -> IoResult<()> {
        let trace_events: Vec<TraceEvent> = outcomes
            .iter()
            .map(|outcome: &TaskOutcome| -> TraceEvent {
                TraceEvent {
                    name: outcome.task().name().to_string(),
                    ph: "X",
                    ts: build_start.elapsed_until(outcome.task_start()),
                    dur: outcome.task_duration(),
                    pid: 0,
                    tid: outcome.fiber(),
                    args: TraceArgs { cache: "miss" },
                }
            })
            .collect();
        let trace_file: TraceFile = TraceFile { trace_events };
        let json: String = serde_json::to_string_pretty(&trace_file).expect("telemetry serialisation is infallible");
        file_system.create_directories(build_directory.as_ref())?;
        let telemetry_path: AbsoluteFile = build_directory.join_file(&RelativeFile::new("telemetry.json"));
        file_system.write(telemetry_path.as_ref(), json.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugin::Task;
    use crate::plugin::TaskName;
    use crate::runtime::DummyRuntime;
    use crate::runtime::Runtime;
    use crate::types::CommandOutput;
    use crate::types::ShellCommand;
    use crate::types::Stderr;
    use crate::types::Stdout;
    use crate::types::Step;
    use crate::types::TaskStart;
    use crate::types::TaskStatus;
    use std::path::Path;
    use std::path::PathBuf;
    use std::time::Duration;

    const BUILD_DIRECTORY: &str = "/build";

    fn build_directory() -> AbsoluteDirectory {
        AbsoluteDirectory::new(PathBuf::from(BUILD_DIRECTORY))
    }

    fn make_outcome(task_name: &str, fiber: Fiber, task_duration: Duration, task_start: TaskStart) -> TaskOutcome {
        TaskOutcome::new(
            Task::new(
                TaskName::new(task_name),
                Step::new("compile"),
                ShellCommand::new("true"),
                vec![],
                vec![],
            ),
            CommandOutput::new(Stdout::default(), Stderr::default(), TaskStatus::Succeeded),
            task_duration,
            task_start,
            fiber,
        )
    }

    fn write_and_parse(outcomes: &[TaskOutcome], build_start: BuildStart, runtime: &DummyRuntime) -> serde_json::Value {
        Telemetry::write(outcomes, &build_directory(), build_start, runtime).unwrap();
        let bytes: Vec<u8> = runtime
            .written_file(Path::new(BUILD_DIRECTORY).join("telemetry.json"))
            .expect("telemetry.json should have been written");
        serde_json::from_slice(&bytes).unwrap()
    }

    #[test]
    fn telemetry_json_contains_one_event_per_task() {
        let build_start: BuildStart = BuildStart::now();
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task_start: TaskStart = TaskStart::new(runtime.now());
        let outcomes: Vec<TaskOutcome> = vec![
            make_outcome("go-compile", Fiber::default(), Duration::from_millis(100), task_start),
            make_outcome("go-test", Fiber::default(), Duration::from_millis(200), task_start),
        ];
        let parsed: serde_json::Value = write_and_parse(&outcomes, build_start, &runtime);
        let events: &Vec<serde_json::Value> = parsed["traceEvents"].as_array().unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["name"], "go-compile");
        assert_eq!(events[1]["name"], "go-test");
    }

    #[test]
    fn all_dur_values_are_positive() {
        let build_start: BuildStart = BuildStart::now();
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task_start: TaskStart = TaskStart::new(runtime.now());
        let outcomes: Vec<TaskOutcome> = vec![
            make_outcome("go-compile", Fiber::default(), Duration::from_millis(100), task_start),
            make_outcome("go-test", Fiber::default(), Duration::from_millis(50), task_start),
        ];
        let parsed: serde_json::Value = write_and_parse(&outcomes, build_start, &runtime);
        for event in parsed["traceEvents"].as_array().unwrap() {
            assert!(
                event["dur"].as_u64().unwrap() > 0,
                "dur should be positive; got: {event}"
            );
        }
    }

    #[test]
    fn parallel_tasks_have_distinct_tid_values() {
        let build_start: BuildStart = BuildStart::now();
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task_start: TaskStart = TaskStart::new(runtime.now());
        let outcomes: Vec<TaskOutcome> = vec![
            make_outcome("task-a", Fiber::new(0), Duration::from_millis(100), task_start),
            make_outcome("task-b", Fiber::new(1), Duration::from_millis(100), task_start),
        ];
        let parsed: serde_json::Value = write_and_parse(&outcomes, build_start, &runtime);
        let events: &Vec<serde_json::Value> = parsed["traceEvents"].as_array().unwrap();
        let tid_a: u64 = events[0]["tid"].as_u64().unwrap();
        let tid_b: u64 = events[1]["tid"].as_u64().unwrap();
        assert_ne!(tid_a, tid_b, "parallel tasks should have distinct tid values");
    }

    #[test]
    fn telemetry_creates_the_build_directory() {
        let build_start: BuildStart = BuildStart::now();
        let runtime: DummyRuntime = DummyRuntime::builder().build();
        let task_start: TaskStart = TaskStart::new(runtime.now());
        let outcomes: Vec<TaskOutcome> = vec![make_outcome(
            "go-compile",
            Fiber::default(),
            Duration::from_millis(100),
            task_start,
        )];
        Telemetry::write(&outcomes, &build_directory(), build_start, &runtime).unwrap();
        assert!(runtime.created_directory(BUILD_DIRECTORY));
    }
}
