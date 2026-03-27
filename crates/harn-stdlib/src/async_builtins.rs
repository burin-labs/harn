use std::sync::atomic::Ordering;
use std::sync::Arc;

use harn_runtime::{AtomicHandle, ChannelHandle, Interpreter, RuntimeError, Value};

/// Register async builtins (HTTP, sleep) on an interpreter.
pub fn register_async_builtins(interp: &mut Interpreter) {
    interp.register_async_builtin("http_get", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_get: URL is required"));
        }
        let response = reqwest::get(&url)
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_get failed: {e}")))?;
        let text = response
            .text()
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_get read failed: {e}")))?;
        Ok(Value::String(text))
    });

    interp.register_async_builtin("http_post", |args| async move {
        let url = args.first().map(|a| a.as_string()).unwrap_or_default();
        let body = args.get(1).map(|a| a.as_string()).unwrap_or_default();
        if url.is_empty() {
            return Err(RuntimeError::thrown("http_post: URL is required"));
        }

        let mut headers = std::collections::BTreeMap::new();
        if let Some(Value::Dict(h)) = args.get(2) {
            for (k, v) in h {
                headers.insert(k.clone(), v.as_string());
            }
        }

        let client = reqwest::Client::new();
        let mut req = client.post(&url).body(body);
        for (k, v) in &headers {
            req = req.header(k.as_str(), v.as_str());
        }

        let response = req
            .send()
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_post failed: {e}")))?;
        let text = response
            .text()
            .await
            .map_err(|e| RuntimeError::thrown(format!("http_post read failed: {e}")))?;
        Ok(Value::String(text))
    });

    interp.register_async_builtin("sleep", |args| async move {
        let ms = match args.first() {
            Some(Value::Duration(ms)) => *ms,
            Some(Value::Int(n)) => *n as u64,
            _ => 0,
        };
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
        Ok(Value::Nil)
    });

    // prompt_user(message) — reads a line from stdin (for CLI interactive mode)
    interp.register_builtin("prompt_user", |args, out| {
        let message = args.first().map(|a| a.as_string()).unwrap_or_default();
        if !message.is_empty() {
            out.extend_from_slice(message.as_bytes());
            out.extend_from_slice(b"\n");
        }
        let mut input = String::new();
        std::io::stdin()
            .read_line(&mut input)
            .map_err(|e| RuntimeError::thrown(format!("Failed to read input: {e}")))?;
        Ok(Value::String(input.trim_end().to_string()))
    });

    // --- Real channel implementation ---

    // channel(name?, capacity?) — creates a real async channel
    interp.register_builtin("channel", |args, _out| {
        let name = args
            .first()
            .map(|a| a.as_string())
            .unwrap_or("default".into());
        let capacity = args.get(1).and_then(|a| a.as_int()).unwrap_or(256) as usize;
        let capacity = capacity.max(1);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        Ok(Value::Channel(ChannelHandle {
            name,
            sender: Arc::new(tx),
            receiver: Arc::new(tokio::sync::Mutex::new(rx)),
            closed: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }))
    });

    // send(channel, value) — sends a value on a channel, returns false if closed
    interp.register_async_builtin("send", |args| async move {
        if args.len() < 2 {
            return Err(RuntimeError::thrown("send: requires channel and value"));
        }
        if let Value::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                return Ok(Value::Bool(false));
            }
            let val = args[1].clone();
            match ch.sender.send(val).await {
                Ok(()) => Ok(Value::Bool(true)),
                Err(_) => Ok(Value::Bool(false)),
            }
        } else {
            Err(RuntimeError::thrown(
                "send: first argument must be a channel",
            ))
        }
    });

    // receive(channel) — receives the next value from a channel (blocking)
    // Returns nil if channel is closed and empty.
    interp.register_async_builtin("receive", |args| async move {
        if args.is_empty() {
            return Err(RuntimeError::thrown("receive: requires a channel"));
        }
        if let Value::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                // Drain remaining buffered messages
                let mut rx = ch.receiver.lock().await;
                return match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(Value::Nil),
                };
            }
            let mut rx = ch.receiver.lock().await;
            match rx.recv().await {
                Some(val) => Ok(val),
                None => Ok(Value::Nil),
            }
        } else {
            Err(RuntimeError::thrown(
                "receive: first argument must be a channel",
            ))
        }
    });

    // try_receive(channel) — non-blocking receive, returns nil if empty
    interp.register_builtin("try_receive", |args, _out| {
        if args.is_empty() {
            return Err(RuntimeError::thrown("try_receive: requires a channel"));
        }
        if let Value::Channel(ch) = &args[0] {
            match ch.receiver.try_lock() {
                Ok(mut rx) => match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(Value::Nil),
                },
                Err(_) => Ok(Value::Nil),
            }
        } else {
            Err(RuntimeError::thrown(
                "try_receive: first argument must be a channel",
            ))
        }
    });

    // close_channel(channel) — marks channel as closed; send() returns false after this
    interp.register_builtin("close_channel", |args, _out| {
        if args.is_empty() {
            return Err(RuntimeError::thrown("close_channel: requires a channel"));
        }
        if let Value::Channel(ch) = &args[0] {
            ch.closed.store(true, Ordering::SeqCst);
            Ok(Value::Nil)
        } else {
            Err(RuntimeError::thrown(
                "close_channel: first argument must be a channel",
            ))
        }
    });

    // --- Real atomic implementation ---

    // atomic(initial?) — creates a real atomic integer
    interp.register_builtin("atomic", |args, _out| {
        let initial = match args.first() {
            Some(Value::Int(n)) => *n,
            Some(Value::Float(f)) => *f as i64,
            Some(Value::Bool(b)) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        };
        Ok(Value::Atomic(AtomicHandle {
            value: Arc::new(std::sync::atomic::AtomicI64::new(initial)),
        }))
    });

    // atomic_get(a) — read the current value atomically
    interp.register_builtin("atomic_get", |args, _out| {
        if let Some(Value::Atomic(a)) = args.first() {
            Ok(Value::Int(a.value.load(Ordering::SeqCst)))
        } else {
            Ok(Value::Nil)
        }
    });

    // atomic_set(a, value) — set the value atomically, returns the old value
    interp.register_builtin("atomic_set", |args, _out| {
        if args.len() >= 2 {
            if let (Value::Atomic(a), Some(val)) = (&args[0], args[1].as_int()) {
                let old = a.value.swap(val, Ordering::SeqCst);
                return Ok(Value::Int(old));
            }
        }
        Ok(Value::Nil)
    });

    // atomic_add(a, n) — atomically add n and return the previous value
    interp.register_builtin("atomic_add", |args, _out| {
        if args.len() >= 2 {
            if let (Value::Atomic(a), Some(delta)) = (&args[0], args[1].as_int()) {
                let prev = a.value.fetch_add(delta, Ordering::SeqCst);
                return Ok(Value::Int(prev));
            }
        }
        Ok(Value::Nil)
    });

    // atomic_cas(a, expected, new) — compare-and-swap, returns bool (success)
    interp.register_builtin("atomic_cas", |args, _out| {
        if args.len() >= 3 {
            if let (Value::Atomic(a), Some(expected), Some(new_val)) =
                (&args[0], args[1].as_int(), args[2].as_int())
            {
                let result =
                    a.value
                        .compare_exchange(expected, new_val, Ordering::SeqCst, Ordering::SeqCst);
                return Ok(Value::Bool(result.is_ok()));
            }
        }
        Ok(Value::Bool(false))
    });

    // --- Select (multiplex channels) ---

    // select(channel1, channel2, ...) — wait for first available message
    // Returns a dict: { "index": <which channel>, "value": <received value> }
    interp.register_async_builtin("select", |args| async move {
        if args.is_empty() {
            return Err(RuntimeError::thrown(
                "select: requires at least one channel",
            ));
        }

        let mut channels: Vec<&ChannelHandle> = Vec::new();
        for arg in &args {
            if let Value::Channel(ch) = arg {
                channels.push(ch);
            } else {
                return Err(RuntimeError::thrown(
                    "select: all arguments must be channels",
                ));
            }
        }

        // Poll channels in a loop until one has data
        // We use try_recv first for fairness, then async recv via tokio::select
        loop {
            // First try non-blocking receives
            for (i, ch) in channels.iter().enumerate() {
                if let Ok(mut rx) = ch.receiver.try_lock() {
                    if let Ok(val) = rx.try_recv() {
                        let mut result = std::collections::BTreeMap::new();
                        result.insert("index".to_string(), Value::Int(i as i64));
                        result.insert("value".to_string(), val);
                        result.insert("channel".to_string(), Value::String(ch.name.clone()));
                        return Ok(Value::Dict(result));
                    }
                }
            }
            // Brief sleep to avoid burning CPU while polling
            tokio::time::sleep(tokio::time::Duration::from_micros(100)).await;
        }
    });
}
