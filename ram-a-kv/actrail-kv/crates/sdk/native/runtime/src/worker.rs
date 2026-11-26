//! 在库内专用线程持有业务实例和 Tokio 运行时，阻塞宿主直到操作完成。
use crate::{Config, Error, Result, Runtime};
use serde_json::Value;
use std::{
    panic::{catch_unwind, AssertUnwindSafe},
    sync::mpsc,
    thread,
};

pub struct CallResult {
    pub data: Value,
    pub source: Option<Value>,
}
impl From<Value> for CallResult {
    fn from(data: Value) -> Self {
        Self { data, source: None }
    }
}

pub trait Application: Sized + Send + 'static {
    fn create(config: Config, runtime: &Runtime) -> Result<Self>;
    fn call(&mut self, runtime: &Runtime, input: &[u8], options: &[u8]) -> Result<Value>;
    fn source(&self) -> Option<Value> {
        None
    }
    fn export(_input: &[u8], _destination: &[u8]) -> Result<Value> {
        Err(Error::input("export is not supported"))
    }
}
struct Command {
    input: Vec<u8>,
    options: Vec<u8>,
    reply: mpsc::SyncSender<Result<CallResult>>,
}
pub struct Worker<A: Application> {
    sender: Option<mpsc::Sender<Command>>,
    thread: Option<thread::JoinHandle<()>>,
    marker: std::marker::PhantomData<A>,
}
impl<A: Application> Worker<A> {
    pub fn create(input: Vec<u8>) -> Result<Self> {
        let (sender, receiver) = mpsc::channel::<Command>();
        let (ready, result) = mpsc::sync_channel(1);
        let thread = thread::Builder::new()
            .name("actrail-sdk".into())
            .spawn(move || {
                let initialized = catch_unwind(AssertUnwindSafe(|| {
                    let config = Config::parse(&input)?;
                    let runtime = tokio::runtime::Builder::new_multi_thread()
                        .worker_threads(2)
                        .enable_all()
                        .build()
                        .map_err(Error::internal)?;
                    let app = A::create(config, &runtime)?;
                    Ok::<_, Error>((runtime, app))
                }))
                .unwrap_or_else(|_| Err(Error::internal("panic during instance creation")));
                let (runtime, mut app) = match initialized {
                    Ok(parts) => {
                        let _ = ready.send(Ok(()));
                        parts
                    }
                    Err(error) => {
                        let _ = ready.send(Err(error));
                        return;
                    }
                };
                let mut poisoned = false;
                while let Ok(command) = receiver.recv() {
                    let result = if poisoned {
                        Err(Error::internal("instance is poisoned by an earlier panic"))
                    } else {
                        match catch_unwind(AssertUnwindSafe(|| {
                            app.call(&runtime, &command.input, &command.options)
                                .map(|data| CallResult {
                                    data,
                                    source: app.source(),
                                })
                        })) {
                            Ok(result) => result,
                            Err(_) => {
                                poisoned = true;
                                Err(Error::internal("panic during operation"))
                            }
                        }
                    };
                    let _ = command.reply.send(result);
                }
                // 先释放业务和存储，再关闭运行时，确保动态库不留下后台任务。
                drop(app);
                drop(runtime);
            })
            .map_err(Error::internal)?;
        match result.recv().map_err(Error::internal)? {
            Ok(()) => Ok(Self {
                sender: Some(sender),
                thread: Some(thread),
                marker: std::marker::PhantomData,
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }
    pub fn call(&self, input: Vec<u8>, options: Vec<u8>) -> Result<CallResult> {
        let (reply, result) = mpsc::sync_channel(1);
        self.sender
            .as_ref()
            .ok_or_else(|| Error::internal("instance is closed"))?
            .send(Command {
                input,
                options,
                reply,
            })
            .map_err(Error::internal)?;
        result.recv().map_err(Error::internal)?
    }
    pub fn close(mut self) -> Result<()> {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            thread
                .join()
                .map_err(|_| Error::internal("panic while closing instance"))?;
        }
        Ok(())
    }
}
impl<A: Application> Drop for Worker<A> {
    fn drop(&mut self) {
        self.sender.take();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
