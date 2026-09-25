// Bonjour publish/browse for P2P LAN sync -- the iOS counterpart of
// NativeBridgePlugin.kt's NsdManager code, reached from
// NativeBridgePlugin.swift's registerP2pService/unregisterP2pService/
// discoverP2pServices and, on the Rust side, p2p_sync.rs's iOS arms of
// `advertise_self`/`browse_lan`.
//
// ## Why NetService and not Network.framework
//
// `NWListener(service:using:)` is Network.framework's only advertiser, and it
// binds its own socket -- but p2p_sync.rs's tokio listener already owns port
// 47811, so the two would fight over it. `NetService` is the API that
// advertises a port something else owns, which is exactly the split here.
// Browsing follows the same choice: `NWBrowser` hands back an endpoint plus
// TXT but no IP address, and `browse_lan` has to return real `SocketAddr`s
// for `resolve_peer` -> `TcpStream::connect` -- getting one out of an
// NWEndpoint means opening a throwaway NWConnection to every peer on the LAN
// just to read its address back. `NetService.resolve(withTimeout:)` gives
// `addresses` + `txtRecordData()` directly, a near-exact match for
// `NsdServiceInfo`'s shape, so both mobile platforms can hand Rust the same
// `{"devices": [...]}` JSON and share one parser (`p2p_sync::parse_discovered`).
//
// Neither API needs `com.apple.developer.networking.multicast`; both go
// through mDNSResponder rather than raw multicast sockets (which is what
// ruled out the `mdns-sd` crate desktop uses). What they do need is
// `NSLocalNetworkUsageDescription` (publish, browse and the raw TCP socket)
// and `NSBonjourServices` (browse only) in gen/apple/project.yml.
//
// NetService is soft-deprecated but has no removal date and is a thin wrapper
// over dns_sd.h, which is also what Network.framework sits on. If it is ever
// withdrawn, the fallback is calling DNSServiceRegister/Browse/Resolve from
// Rust via `extern "C"` and dropping this file entirely.
//
// ## Threading
//
// NativeBridgePlugin.swift's header rule (never hop to the main thread, since
// Tauri blocks the calling Rust thread and that thread is sometimes the main
// one) extends here into a second rule: nothing may depend on the *caller's*
// run loop either. NetService/NetServiceBrowser only deliver delegate
// callbacks on a run loop they're scheduled in, and Tauri's ipc-queue call is
// a one-shot with no run loop of its own. So every Bonjour object lives on
// the single long-lived `BonjourRunLoop` thread below, which also means none
// of the state in this file needs locking -- it is only ever touched there.

import Foundation
import Tauri

private let logTag = "Reflectodoro/P2pSync"

/// Must agree with p2p_sync.rs's `SERVICE_TYPE` ("_reflectodoro._tcp.local.")
/// and NativeBridgePlugin.kt's `P2P_SERVICE_TYPE` ("_reflectodoro._tcp."), or
/// the three platforms stop seeing each other. NetService splits the type and
/// the domain into separate arguments, which is why the ".local." suffix that
/// the Rust constant carries is absent from the type here.
private let serviceType = "_reflectodoro._tcp."
private let serviceDomain = "local."

/// One process-wide thread running a real run loop, created on first use and
/// never torn down (the advertisement is meant to stay up for the process
/// lifetime, same as desktop's and Android's).
final class BonjourRunLoop {
  static let shared = BonjourRunLoop()

  private var cfRunLoop: CFRunLoop!

  private init() {
    let ready = DispatchSemaphore(value: 0)
    let thread = Thread { [self] in
      cfRunLoop = CFRunLoopGetCurrent()
      // A run loop with no input sources returns from run() immediately. An
      // otherwise-unused Mach port is the cheapest way to keep it alive.
      RunLoop.current.add(NSMachPort(), forMode: .default)
      ready.signal()
      while true {
        RunLoop.current.run(mode: .default, before: .distantFuture)
      }
    }
    thread.name = "reflectodoro.bonjour"
    thread.qualityOfService = .utility
    thread.start()
    // Blocks only until the thread has published its run loop -- microseconds,
    // and only on the very first call.
    ready.wait()
  }

  /// Runs `block` on the Bonjour thread. Returns immediately; the caller (a
  /// Tauri ipc-queue thread) is never blocked on Bonjour work.
  func perform(_ block: @escaping () -> Void) {
    CFRunLoopPerformBlock(cfRunLoop, CFRunLoopMode.defaultMode.rawValue, block)
    // Required: PerformBlock queues the block but does not wake a run loop
    // that is already asleep in run(before: .distantFuture).
    CFRunLoopWakeUp(cfRunLoop)
  }
}

/// Decodes one `NetService.addresses` entry into a numeric host string.
/// Returns the address family alongside it so the caller can prefer IPv4.
private func hostString(from data: Data) -> (family: Int32, host: String)? {
  return data.withUnsafeBytes { raw -> (Int32, String)? in
    guard let base = raw.baseAddress, raw.count >= MemoryLayout<sockaddr>.size else {
      return nil
    }
    let sa = base.assumingMemoryBound(to: sockaddr.self)
    let family = Int32(sa.pointee.sa_family)
    guard family == AF_INET || family == AF_INET6 else { return nil }
    var buffer = [CChar](repeating: 0, count: Int(NI_MAXHOST))
    let rc = getnameinfo(
      sa, socklen_t(raw.count), &buffer, socklen_t(buffer.count), nil, 0, NI_NUMERICHOST)
    guard rc == 0 else { return nil }
    return (family, String(cString: buffer))
  }
}

/// Picks the address `browse_lan` should hand Rust, preferring IPv4 for the
/// same reason the desktop arm does: a peer can advertise both families and
/// the IPv6 entry is sometimes globally routable rather than LAN-local.
/// Link-local IPv6 is skipped outright rather than passed along -- getnameinfo
/// renders it with a scope suffix ("fe80::1%en0") that Rust's `IpAddr` parser
/// rejects, so it would only ever be silently dropped on the far side.
private func preferredHost(from addresses: [Data]) -> String? {
  var fallback: String?
  for data in addresses {
    guard let (family, host) = hostString(from: data) else { continue }
    if family == AF_INET {
      return host
    }
    if fallback == nil && !host.lowercased().hasPrefix("fe80:") {
      fallback = host
    }
  }
  return fallback
}

private func txtValue(_ txt: [String: Data], _ key: String) -> String {
  guard let data = txt[key] else { return "" }
  return String(decoding: data, as: UTF8.self)
}

/// One `discoverP2pServices` call: a fixed-window browse burst, resolving
/// every instance it finds and reporting whatever resolved when the window
/// closes. Mirrors the Kotlin implementation's shape, minus its serialized
/// resolve queue -- NsdManager allows only one resolveService() in flight,
/// NetService has no such restriction.
private final class DiscoverSession: NSObject, NetServiceBrowserDelegate, NetServiceDelegate {
  private let browser = NetServiceBrowser()
  /// Strong references are mandatory: a NetService handed to didFind and not
  /// retained deallocates immediately and never calls back.
  private var pending: [NetService] = []
  private var seen = Set<String>()
  private var results: [[String: Any]] = []
  private let invoke: Invoke
  private let deadline: TimeInterval
  private var timer: Timer?
  private var done = false
  private let onFinish: (DiscoverSession) -> Void

  init(invoke: Invoke, timeoutMs: Int, onFinish: @escaping (DiscoverSession) -> Void) {
    self.invoke = invoke
    self.deadline = max(TimeInterval(timeoutMs) / 1000.0, 0.1)
    self.onFinish = onFinish
    super.init()
  }

  /// Called on the Bonjour thread, so `RunLoop.current` is that thread's.
  func start() {
    browser.delegate = self
    browser.schedule(in: RunLoop.current, forMode: .default)
    timer = Timer.scheduledTimer(withTimeInterval: deadline, repeats: false) { [weak self] _ in
      self?.finish()
    }
    browser.searchForServices(ofType: serviceType, inDomain: serviceDomain)
  }

  private func finish() {
    guard !done else { return }
    done = true
    timer?.invalidate()
    timer = nil
    browser.stop()
    browser.remove(from: RunLoop.current, forMode: .default)
    for service in pending {
      service.stop()
    }
    pending.removeAll()
    NSLog("%@", "\(logTag): Bonjour browse finished with \(results.count) device(s)")
    invoke.resolve(["devices": results])
    onFinish(self)
  }

  // --- NetServiceBrowserDelegate ---

  func netServiceBrowser(
    _ browser: NetServiceBrowser, didFind service: NetService, moreComing: Bool
  ) {
    guard !done, seen.insert(service.name).inserted else { return }
    pending.append(service)
    service.delegate = self
    service.schedule(in: RunLoop.current, forMode: .default)
    service.resolve(withTimeout: deadline)
  }

  func netServiceBrowser(_ browser: NetServiceBrowser, didNotSearch errorDict: [String: NSNumber]) {
    NSLog("%@", "\(logTag): Bonjour browse failed to start: \(errorDict)")
    finish()
  }

  // --- NetServiceDelegate ---

  func netServiceDidResolveAddress(_ service: NetService) {
    guard !done else { return }
    let txt = NetService.dictionary(fromTXTRecord: service.txtRecordData() ?? Data())
    let deviceId = txtValue(txt, "device_id")
    guard !deviceId.isEmpty, let host = preferredHost(from: service.addresses ?? []) else {
      return
    }
    results.append([
      "deviceId": deviceId,
      "name": txtValue(txt, "name"),
      "platform": txtValue(txt, "platform"),
      "host": host,
      "port": service.port,
    ])
  }

  func netService(_ service: NetService, didNotResolve errorDict: [String: NSNumber]) {
    // Expected for a peer that went away mid-browse; the burst just reports
    // whatever else resolved.
    NSLog("%@", "\(logTag): Bonjour resolve failed for \(service.name): \(errorDict)")
  }
}

final class P2pBonjour: NSObject, NetServiceDelegate {
  static let shared = P2pBonjour()

  private var published: NetService?
  private var sessions: [DiscoverSession] = []

  /// Unregister-before-register, so repeat calls are idempotent -- the same
  /// contract desktop's and Android's advertise_self rely on, and what makes
  /// `resync_advertised_name` able to correct a cold-start blank device name.
  func publish(deviceId: String, name: String, platform: String, port: Int) {
    BonjourRunLoop.shared.perform { [self] in
      published?.stop()
      published = nil

      let service = NetService(
        domain: serviceDomain, type: serviceType, name: deviceId, port: Int32(port))
      let txt: [String: Data] = [
        "device_id": Data(deviceId.utf8),
        "name": Data(name.utf8),
        "platform": Data(platform.utf8),
      ]
      _ = service.setTXTRecord(NetService.data(fromTXTRecord: txt))
      service.delegate = self
      service.schedule(in: RunLoop.current, forMode: .default)
      // publish() alone, never .listenForConnections -- that variant would
      // bind its own socket and collide with p2p_sync.rs's tokio listener.
      service.publish()
      published = service
    }
  }

  func unpublish() {
    BonjourRunLoop.shared.perform { [self] in
      published?.stop()
      published = nil
    }
  }

  /// Resolves `invoke` later, from the browse window's timer.
  func discover(invoke: Invoke, timeoutMs: Int) {
    BonjourRunLoop.shared.perform { [self] in
      let session = DiscoverSession(invoke: invoke, timeoutMs: timeoutMs) { [weak self] finished in
        self?.sessions.removeAll { $0 === finished }
      }
      sessions.append(session)
      session.start()
    }
  }

  // --- NetServiceDelegate (publishing only) ---
  //
  // Logged rather than reported back through the Invoke: publish() is async,
  // and registerP2pService has already resolved by the time these fire, so
  // the log is the only place the outcome can surface. Same reasoning (and
  // same tag) as the Kotlin RegistrationListener.

  func netServiceDidPublish(_ service: NetService) {
    NSLog("%@", "\(logTag): Bonjour service published: \(service.name)")
  }

  func netService(_ service: NetService, didNotPublish errorDict: [String: NSNumber]) {
    NSLog("%@", "\(logTag): Bonjour publish failed for \(service.name): \(errorDict)")
  }
}
