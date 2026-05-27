use bytes::Bytes;
use http_body_util::Full;
use hyper::server::conn::http1;
use hyper::service::Service;
use hyper::{Request,Response};
use hyper::{body::Incoming as IncomingBody};
use sd_notify::{notify,NotifyState};
use std::collections::VecDeque;
use std::env;
use std::fs::File;
use std::future::Future;
use std::io::BufReader;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tokio::net::TcpListener;

use hyper_util::rt::{TokioIo,TokioTimer};

struct TargetStats {
  target: String,
  samples: VecDeque<u32>
}

impl TargetStats {
  pub fn new(target: impl Into<String>) -> TargetStats {
    TargetStats { target: target.into(), samples: VecDeque::new() }
  }
  fn get_min(&self) -> u32 {
    *self.samples.iter().min().unwrap_or(&0u32)
  }
  fn get_avg(&self) -> u32 {
    self.samples.iter().sum::<u32>() / (self.samples.len() as u32)
  }
  fn get_max(&self) -> u32 {
    *self.samples.iter().max().unwrap_or(&0u32)
  }
  pub fn add_sample(&mut self, rtt: u32) {
    self.samples.push_back(rtt);
    if self.samples.len() >= 60 {
      self.samples.pop_front();
    }
  }
  pub fn get_metrics(&self) -> String {
    format!("ping_min{{target=\"{}\"}} {}\nping_avg{{target=\"{}\"}} {}\nping_max{{target=\"{}\"}} {}", self.target, self.get_min(), self.target, self.get_avg(), self.target, self.get_max())
  }
}

fn do_ping_target(target: &str) -> Option<u32> {
  let addr = target.parse().unwrap();
  let data = [1,2,3,4];  // ping data
  let timeout = Duration::from_secs(1);
  let options = ping_rs::PingOptions { ttl: 128, dont_fragment: true };
  let result = ping_rs::send_ping(&addr, timeout, &data, Some(&options));
  match result {
    Ok(reply) => {
      // println!("Reply from {}: bytes={} time={}ms", reply.address, data.len(), reply.rtt);
      Some(reply.rtt)
    }
    Err(e) => {
      println!("{} {:?}", target, e);
      None
    }
  }
}

fn spawn_worker_thread(target: String, output: Arc<Mutex<TargetStats>>) {
  let target = target.to_string();
  thread::spawn(move || {
    loop {
      {
        let mut t = output.lock().unwrap();

        let res = do_ping_target(&target);
        match res {
          Some(rtt) => t.add_sample(rtt),
          None => (),
        }
      }
      // dont sleep while t is in scope, it holds the lock
      thread::sleep(Duration::from_millis(1000));
    }
  });
}

#[derive(Clone)]
struct Svc {
  outputs: Vec<Arc<Mutex<TargetStats>>>
}

impl Service<Request<IncomingBody>> for Svc {
  type Response = Response<Full<Bytes>>;
  type Error = hyper::Error;
  type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;
  fn call(&self, _req: Request<IncomingBody>) -> Self::Future {
    let mut output: String = String::new();
    for o in &self.outputs {
      let t = o.lock().unwrap();
      output.push_str((*t).get_metrics().as_str());
      output.push_str("\n");
    }
    Box::pin(async { Ok(Response::new(Full::new(Bytes::from(output)))) } )
  }
}

#[tokio::main]
pub async fn main() -> Result<(),Box<dyn std::error::Error + Send + Sync>> {
  let targets: Vec<String> = match env::var("CONFIG_FILE") {
    Ok(path) => {
      let file = File::open(path)?;
      let reader = BufReader::new(file);
      serde_json::from_reader(reader)?
    }
    Err(e) => panic!("couldnt find env var CONFIG_FILE: {}", e)
  };

  let mut outputs : Vec<Arc<Mutex<TargetStats>>> = Vec::new();
  for target in targets {
    let output = Arc::new(Mutex::new(TargetStats::new(&target)));
    spawn_worker_thread(target, output.clone());
    outputs.push(output);
  }
  let svc = Svc {
    outputs: outputs
  };
  let addr: SocketAddr = ([0,0,0,0],3000).into();
  let listener:TcpListener = TcpListener::bind(addr).await?;
  let _ = notify(&[NotifyState::Ready]);
  loop {
    let (tcp,_) = listener.accept().await?;
    let io = TokioIo::new(tcp);
    let svc_clone = svc.clone();
    tokio::task::spawn(async move {
      match http1::Builder::new().timer(TokioTimer::new()).serve_connection(io, svc_clone).await {
        Err(err) => println!("error serving connection {:?}", err),
        Ok(_) => {
          let _ = notify(&[NotifyState::Watchdog]);
        },
      };
    });
  }
}
