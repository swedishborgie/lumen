/**
 * lumen-client.js — WebRTC connection library for Lumen.
 *
 * Manages signaling, ICE, media tracks, and the input data channel.
 * Framework-agnostic; communicates via CustomEvents on EventTarget.
 *
 * Events dispatched:
 *   statuschange  — { detail: string }
 *   statechange   — { detail: 'idle'|'connecting'|'connected'|'disconnected' }
 *   track         — { detail: MediaStreamTrack }
 */

export const KEY_MAP = {
  Escape:1,Backquote:41,Digit1:2,Digit2:3,Digit3:4,Digit4:5,Digit5:6,
  Digit6:7,Digit7:8,Digit8:9,Digit9:10,Digit0:11,Minus:12,Equal:13,
  Backspace:14,Tab:15,KeyQ:16,KeyW:17,KeyE:18,KeyR:19,KeyT:20,KeyY:21,
  KeyU:22,KeyI:23,KeyO:24,KeyP:25,BracketLeft:26,BracketRight:27,
  Enter:28,ControlLeft:29,KeyA:30,KeyS:31,KeyD:32,KeyF:33,KeyG:34,
  KeyH:35,KeyJ:36,KeyK:37,KeyL:38,Semicolon:39,Quote:40,ShiftLeft:42,
  Backslash:43,KeyZ:44,KeyX:45,KeyC:46,KeyV:47,KeyB:48,KeyN:49,KeyM:50,
  Comma:51,Period:52,Slash:53,ShiftRight:54,NumpadMultiply:55,AltLeft:56,
  Space:57,CapsLock:58,F1:59,F2:60,F3:61,F4:62,F5:63,F6:64,F7:65,F8:66,
  F9:67,F10:68,NumLock:69,ScrollLock:70,Numpad7:71,Numpad8:72,Numpad9:73,
  NumpadSubtract:74,Numpad4:75,Numpad5:76,Numpad6:77,NumpadAdd:78,
  Numpad1:79,Numpad2:80,Numpad3:81,Numpad0:82,NumpadDecimal:83,
  IntlBackslash:86,F11:87,F12:88,NumpadEnter:96,ControlRight:97,
  NumpadDivide:98,PrintScreen:99,AltRight:100,Home:102,ArrowUp:103,
  PageUp:104,ArrowLeft:105,ArrowRight:106,End:107,ArrowDown:108,
  PageDown:109,Insert:110,Delete:111,MetaLeft:125,MetaRight:126,
  ContextMenu:127,
};

// BTN_LEFT=272, BTN_RIGHT=273, BTN_MIDDLE=274
export const BTN_CODES = [272, 273, 274];

export class LumenClient extends EventTarget {
  #pc          = null;
  #ws          = null;
  #dc          = null;
  #stream      = null;
  #sessionId   = null;
  #state       = 'idle';

  get stream()    { return this.#stream; }
  get state()     { return this.#state; }
  get sessionId() { return this.#sessionId; }

  /** Connect to the Lumen server.  signalUrl defaults to the current host. */
  async connect(signalUrl) {
    if (this.#state !== 'idle') return;
    this.#setState('connecting');
    this.#setStatus('Connecting…');

    const url = signalUrl ?? `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws/signal`;

    this.#ws = new WebSocket(url);
    this.#ws.onerror = () => { this.#setStatus('WebSocket error'); this.disconnect(); };
    this.#ws.onclose = () => { this.#setStatus('Signaling closed'); this.disconnect(); };

    await new Promise((resolve, reject) => {
      this.#ws.onopen  = resolve;
      this.#ws.onerror = reject;
    });

    this.#pc = new RTCPeerConnection({
      iceServers: [{ urls: 'stun:stun.l.google.com:19302' }],
      bundlePolicy: 'max-bundle',
    });

    this.#stream = new MediaStream();

    this.#pc.ontrack = (e) => {
      this.#stream.addTrack(e.track);
      this.dispatchEvent(new CustomEvent('track', { detail: e.track }));
    };

    this.#pc.onicecandidate = (e) => {
      if (!e.candidate) return;
      if (this.#ws?.readyState === WebSocket.OPEN) {
        this.#ws.send(JSON.stringify({
          type: 'candidate',
          candidate: e.candidate.candidate,
          sdp_mid: e.candidate.sdpMid,
          sdp_m_line_index: e.candidate.sdpMLineIndex,
        }));
      }
    };

    this.#pc.onconnectionstatechange = () => {
      this.#setStatus(`WebRTC: ${this.#pc.connectionState}`);
      if (['failed', 'closed', 'disconnected'].includes(this.#pc.connectionState)) {
        this.disconnect();
      }
    };

    this.#pc.addTransceiver('video', { direction: 'recvonly' });
    this.#pc.addTransceiver('audio', { direction: 'recvonly' });

    this.#dc = this.#pc.createDataChannel('input');
    this.#dc.onopen  = () => {
      this.#setState('connected');
      this.#setStatus('Connected');
    };
    this.#dc.onclose = () => { this.#dc = null; };
    this.#dc.onerror = (e) => { console.error('[lumen-client] data channel error', e); };
    this.#dc.onmessage = (e) => {
      try {
        const msg = JSON.parse(e.data);
        this.dispatchEvent(new CustomEvent('dcmessage', { detail: msg }));
      } catch (err) {
        console.warn('[lumen-client] data channel message parse error', err);
      }
    };

    const offer = await this.#pc.createOffer();
    await this.#pc.setLocalDescription(offer);

    this.#ws.onmessage = async (evt) => {
      const msg = JSON.parse(evt.data);
      if (msg.type === 'answer') {
        this.#sessionId = msg.session_id;
        await this.#pc.setRemoteDescription({ type: 'answer', sdp: msg.sdp });
        this.#setStatus('Waiting for media…');
      } else if (msg.type === 'candidate') {
        await this.#pc.addIceCandidate({ candidate: msg.candidate });
      } else if (msg.type === 'error') {
        this.#setStatus(`Server error: ${msg.message}`);
      }
    };

    this.#ws.send(JSON.stringify({ type: 'offer', sdp: offer.sdp }));
  }

  disconnect() {
    if (this.#state === 'idle') return;
    this.#dc?.close();  this.#dc = null;
    this.#pc?.close();  this.#pc = null;
    this.#ws?.close();  this.#ws = null;
    this.#stream  = null;
    this.#sessionId = null;
    this.#setState('idle');
    this.#setStatus('Idle');
  }

  /**
   * Send a raw input event object to the compositor via the data channel.
   * Silently drops the event if the channel is not open.
   */
  sendInput(obj) {
    if (this.#dc?.readyState === 'open') {
      this.#dc.send(JSON.stringify(obj));
    }
  }

  /**
   * Collect WebRTC stats and return a structured snapshot.
   * Returns null if not connected.
   */
  async getStats() {
    if (!this.#pc) return null;
    const reports = await this.#pc.getStats();
    const snap = {
      videoBytes: 0, videoPackets: 0, videoLost: 0,
      framesDecoded: 0, framesDropped: 0, framesReceived: 0,
      jitter: null, rtt: null, decoderImpl: null,
    };
    reports.forEach(r => {
      if (r.type === 'inbound-rtp' && r.kind === 'video') {
        snap.videoBytes     = r.bytesReceived    ?? 0;
        snap.videoPackets   = r.packetsReceived  ?? 0;
        snap.videoLost      = r.packetsLost      ?? 0;
        snap.framesDecoded  = r.framesDecoded    ?? 0;
        snap.framesDropped  = r.framesDropped    ?? 0;
        snap.framesReceived = r.framesReceived   ?? 0;
        snap.jitter         = r.jitter           ?? null;
        snap.decoderImpl    = r.decoderImplementation ?? null;
      }
      if (r.type === 'remote-inbound-rtp' && r.kind === 'video') {
        snap.rtt = r.roundTripTime ?? null;
      }
    });
    return snap;
  }

  // ── private helpers ──────────────────────────────────────────────────────────

  #setState(s) {
    this.#state = s;
    this.dispatchEvent(new CustomEvent('statechange', { detail: s }));
  }

  #setStatus(msg) {
    this.dispatchEvent(new CustomEvent('statuschange', { detail: msg }));
  }
}
