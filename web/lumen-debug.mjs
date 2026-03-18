/**
 * lumen-debug.mjs — Debug logging for Lumen.
 *
 * Provides a global singleton logger with numeric log levels and subsystem
 * scoping. Level is NONE by default; the UI controls enable and tune it.
 *
 * Usage:
 *   import { logger, Level } from './lumen-debug.mjs';
 *   const log = logger.forSubsystem('client');
 *
 *   logger.setLevel(Level.DEBUG);
 *   log.info('websocket', 'WebSocket opened');
 *   log.verbose('offer', 'SDP:', sdpText);
 */

/** Format elapsed time since page load as a compact `+0.000s` string. */
function elapsedLabel() {
  return `+${(performance.now() / 1000).toFixed(3)}s`;
}

/** Numeric log levels in ascending verbosity order. */
export const Level = Object.freeze({
  NONE:    0,
  ERROR:   1,
  WARN:    2,
  INFO:    3,
  DEBUG:   4,
  VERBOSE: 5,
});

const LEVEL_LABELS = Object.freeze({
  [Level.ERROR]:   'ERROR',
  [Level.WARN]:    'WARN',
  [Level.INFO]:    'INFO',
  [Level.DEBUG]:   'DEBUG',
  [Level.VERBOSE]: 'VERBOSE',
});

// CSS styles applied to the prefix badge for each level.
const LEVEL_STYLES = Object.freeze({
  [Level.ERROR]:   'color:#ff4444;font-weight:bold',
  [Level.WARN]:    'color:#ffaa00;font-weight:bold',
  [Level.INFO]:    'color:#4da6ff',
  [Level.DEBUG]:   'color:#999999',
  [Level.VERBOSE]: 'color:#555555',
});

function emit(level, currentLevel, subsystem, phase, args) {
  if (level > currentLevel) return;
  const label  = LEVEL_LABELS[level];
  const style  = LEVEL_STYLES[level];
  const prefix = `[lumen:${label}] [${elapsedLabel()}] [${subsystem}/${phase}]`;
  if (level === Level.ERROR) {
    console.error(`%c${prefix}`, style, ...args);
  } else if (level === Level.WARN) {
    console.warn(`%c${prefix}`, style, ...args);
  } else {
    console.log(`%c${prefix}`, style, ...args);
  }
}

/**
 * Lightweight scoped logger bound to a subsystem name.
 * Delegates all calls to the parent LumenLogger.
 */
class ScopedLogger {
  #parent;
  #subsystem;

  constructor(parent, subsystem) {
    this.#parent = parent;
    this.#subsystem = subsystem;
  }

  error  (phase, ...args) { emit(Level.ERROR,   this.#parent.getLevel(), this.#subsystem, phase, args); }
  warn   (phase, ...args) { emit(Level.WARN,    this.#parent.getLevel(), this.#subsystem, phase, args); }
  info   (phase, ...args) { emit(Level.INFO,    this.#parent.getLevel(), this.#subsystem, phase, args); }
  debug  (phase, ...args) { emit(Level.DEBUG,   this.#parent.getLevel(), this.#subsystem, phase, args); }
  verbose(phase, ...args) { emit(Level.VERBOSE, this.#parent.getLevel(), this.#subsystem, phase, args); }
}

class LumenLogger {
  #level = Level.NONE;

  /** Set the active log level. Messages above this level are suppressed. */
  setLevel(level) { this.#level = level; }

  /** Return the current log level. */
  getLevel() { return this.#level; }

  /**
   * Return a ScopedLogger whose every call is tagged with `subsystem`.
   * @param {string} subsystem  Short identifier, e.g. 'client', 'ui'.
   */
  forSubsystem(subsystem) {
    return new ScopedLogger(this, subsystem);
  }
}

/** Global singleton logger. Set its level via the debug UI controls. */
export const logger = new LumenLogger();
