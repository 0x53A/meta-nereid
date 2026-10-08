use super::*;
#[test]
fn live_demands_release_beats_before_optical_activation_and_restore_union() {
    use std::os::unix::net::UnixListener;
    use std::io::BufRead;
    use std::sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}};
    let inventory=json!({"sensors":([1,4,18,19,21,31,65561,65574,65572].map(|t|
        json!({"type":t,"handle":t,"flags":1,"min_delay_us":0,"max_delay_us":0})))});
    let token="12345678-1234-1234-1234-123456789abc";
    let root=std::env::temp_dir().join(format!("hoki-broker-{}",std::process::id()));
    fs::create_dir_all(&root).unwrap();
    let socket=root.join("socket");let _=fs::remove_file(&socket);
    let listener=UnixListener::bind(&socket).unwrap();listener.set_nonblocking(true).unwrap();
    let done=Arc::new(AtomicBool::new(false));let log=Arc::new(Mutex::new(Vec::<Value>::new()));
    let end=done.clone();let requests=log.clone();
    let server=std::thread::spawn(move||while !end.load(Ordering::SeqCst) {
        if let Ok((mut stream,_))=listener.accept() {
            let mut line=String::new();std::io::BufReader::new(&stream).read_line(&mut line).unwrap();
            requests.lock().unwrap().push(serde_json::from_str(&line).unwrap());
            writeln!(stream,"{{\"error\":0}}").unwrap();
        } else {std::thread::sleep(std::time::Duration::from_millis(1));}
    });
    let plan=root.join("plan.json");
    let save=|revision,profiles:Value|fs::write(&plan,serde_json::to_vec(&json!({"version":1,"epoch":token,"revision":revision,"profiles":profiles})).unwrap()).unwrap();
    let mut broker=Broker::new(plan.clone(),100.0).unwrap();broker.schedule.deadline=200.0;
    save(1,json!(["running","full"]));
    broker.update(&inventory,&socket,token,&root,100.0).unwrap();
    assert!(broker.current.contains_key(&65572));assert!(broker.current.contains_key(&31));
    log.lock().unwrap().clear();
    broker.update(&inventory,&socket,token,&root,201.0).unwrap();
    let calls=log.lock().unwrap().clone();
    let optical=calls.iter().position(|c|c["handle"]==65561 && c["active"]==true).unwrap();
    for handle in [31,65574] {
        assert!(calls.iter().position(|c|c["handle"]==handle && c["active"]==false).unwrap()<optical);
    }
    log.lock().unwrap().clear();
    broker.update(&inventory,&socket,token,&root,382.0).unwrap();
    let calls=log.lock().unwrap().clone();
    assert_eq!(calls[0]["handle"],65561);assert_eq!(calls[0]["active"],false);
    assert!(broker.current.contains_key(&31));assert!(broker.current.contains_key(&65572));
    broker.buffered_full=true;
    save(2,json!(["full"]));broker.update(&inventory,&socket,token,&root,383.0).unwrap();
    assert!(broker.suspend_capable());
    assert_eq!(broker.maintenance_interval(),8.0);
    assert_eq!(broker.current[&1]["latency_ns"],7_000_000_000u64);
    let mut derived=inventory.clone();
    for sensor in derived["sensors"].as_array_mut().unwrap() {
        if sensor["type"]==21 {sensor["flags"]=json!(3);sensor["fifo_max"]=json!(10000);sensor["fifo_reserved"]=json!(0);}
        if sensor["type"]==18 {sensor["flags"]=json!(7);}
        if sensor["type"]==19 {sensor["flags"]=json!(5);}
    }
    broker.buffered_on_change=true;
    let count=broker.current.len();
    broker.update(&derived,&socket,token,&root,383.1).unwrap();
    assert!(broker.suspend_capable());
    assert_eq!(broker.current.len(),count);
    assert_eq!(broker.current[&21]["latency_ns"],7_000_000_000u64);
    assert_eq!(broker.current[&18]["latency_ns"],0);
    assert_eq!(broker.current[&19]["latency_ns"],0);
    broker.buffered_on_change=false;
    broker.update(&derived,&socket,token,&root,383.2).unwrap();
    assert_eq!(broker.current[&21]["latency_ns"],0);
    broker.buffered_on_change=true;
    let mut nonwake=inventory.clone(); nonwake["sensors"][0]["flags"]=json!(0);
    broker.update(&nonwake,&socket,token,&root,384.0).unwrap();
    assert!(!broker.suspend_capable());
    save(3,json!(["full","running"]));broker.update(&derived,&socket,token,&root,385.0).unwrap();
    assert!(!broker.suspend_capable());assert_eq!(broker.maintenance_interval(),1.0);
    assert!(broker.current.values().all(|s|s["latency_ns"]==0));
    save(4,json!(["off"]));broker.update(&inventory,&socket,token,&root,386.0).unwrap();
    assert!(broker.current.is_empty());
    done.store(true,Ordering::SeqCst);server.join().unwrap();fs::remove_dir_all(root).unwrap();
}
