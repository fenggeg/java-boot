use crate::error::AppResult;

/// 根据 PID 查询该进程监听的所有 TCP 端口（LISTENING 状态）
pub fn ports_for_pid(pid: u32) -> AppResult<Vec<u16>> {
    #[cfg(windows)]
    {
        ports_for_pid_windows(pid)
    }
    #[cfg(not(windows))]
    {
        let _ = pid;
        Ok(vec![])
    }
}

/// 查询所有 LISTENING 端口 → PID 的映射（用于冲突检测）
pub fn all_listening_ports() -> AppResult<Vec<(u16, u32)>> {
    #[cfg(windows)]
    {
        all_listening_ports_windows()
    }
    #[cfg(not(windows))]
    {
        Ok(vec![])
    }
}

#[cfg(windows)]
fn ports_for_pid_windows(pid: u32) -> AppResult<Vec<u16>> {
    let table = build_tcp_table()?;
    let mut ports = vec![];
    for (port, owner_pid) in &table {
        if *owner_pid == pid {
            if !ports.contains(port) {
                ports.push(*port);
            }
        }
    }
    Ok(ports)
}

#[cfg(windows)]
fn all_listening_ports_windows() -> AppResult<Vec<(u16, u32)>> {
    build_tcp_table()
}

#[cfg(windows)]
fn build_tcp_table() -> AppResult<Vec<(u16, u32)>> {
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPTABLE_OWNER_PID,
        MIB_TCP6TABLE_OWNER_PID, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::{ntohs, AF_INET, AF_INET6};

    let mut size: u32 = 0;
    // 第一次调用拿大小
    unsafe {
        GetExtendedTcpTable(
            None,
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
    }

    let mut buf = vec![0u8; size as usize];
    let result = unsafe {
        GetExtendedTcpTable(
            Some(buf.as_mut_ptr() as *mut _),
            &mut size,
            false,
            AF_INET.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        )
    };

    let mut out = vec![];
    if result == 0 {
        let table =
            unsafe { &*(buf.as_ptr() as *const MIB_TCPTABLE_OWNER_PID) };
        let count = table.dwNumEntries as usize;
        let rows = unsafe {
            std::slice::from_raw_parts(
                table.table.as_ptr(),
                count,
            )
        };
        for row in rows {
            let port = unsafe { ntohs(row.dwLocalPort as u16) };
            out.push((port, row.dwOwningPid));
        }
    }

    // IPv6
    let mut size6: u32 = 0;
    unsafe {
        GetExtendedTcpTable(
            None,
            &mut size6,
            false,
            AF_INET6.0 as u32,
            TCP_TABLE_OWNER_PID_LISTENER,
            0,
        );
    }
    if size6 > 0 {
        let mut buf6 = vec![0u8; size6 as usize];
        let r6 = unsafe {
            GetExtendedTcpTable(
                Some(buf6.as_mut_ptr() as *mut _),
                &mut size6,
                false,
                AF_INET6.0 as u32,
                TCP_TABLE_OWNER_PID_LISTENER,
                0,
            )
        };
        if r6 == 0 {
            let table6 =
                unsafe { &*(buf6.as_ptr() as *const MIB_TCP6TABLE_OWNER_PID) };
            let count6 = table6.dwNumEntries as usize;
            let rows6 = unsafe {
                std::slice::from_raw_parts(table6.table.as_ptr(), count6)
            };
            for row in rows6 {
                let port = unsafe { ntohs(row.dwLocalPort as u16) };
                out.push((port, row.dwOwningPid));
            }
        }
    }

    Ok(out)
}

/// 备用方案：解析 netstat 输出（当 Windows API 不可用时）
#[allow(dead_code)]
pub fn ports_for_pid_netstat(pid: u32) -> AppResult<Vec<u16>> {
    let output = std::process::Command::new("netstat")
        .args(["-ano"])
        .output()
        .map_err(|e| crate::error::AppError::Process(format!("netstat 执行失败: {}", e)))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let mut ports = vec![];
    for line in text.lines() {
        let line = line.trim();
        if !line.contains("LISTENING") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        let local = parts[1];
        let owner_pid: u32 = match parts[parts.len() - 1].parse() {
            Ok(v) => v,
            Err(_) => continue,
        };
        if owner_pid != pid {
            continue;
        }
        if let Some(port_str) = local.rsplit(':').next() {
            if let Ok(p) = port_str.parse::<u16>() {
                if !ports.contains(&p) {
                    ports.push(p);
                }
            }
        }
    }
    Ok(ports)
}
