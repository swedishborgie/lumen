/**
 * gamepad-mapper.mjs — Interactive mapping wizard for non-standard gamepads.
 *
 * Opens a modal that walks the user through pressing each button and moving
 * each axis on their controller.  The wizard detects which raw browser index
 * responds and records the mapping, producing a capability declaration
 * compatible with the `gamepad_connected` protocol message.
 *
 * Usage:
 *   const mapper = new GamepadMapper(gamepad, (declaration) => { ... });
 *   mapper.show();
 */

// ── Linux evdev button codes (BTN_*) ─────────────────────────────────────────
const BTN_SOUTH      = 0x130;
const BTN_EAST       = 0x131;
const BTN_NORTH      = 0x133;
const BTN_WEST       = 0x134;
const BTN_TL         = 0x136;
const BTN_TR         = 0x137;
const BTN_TL2        = 0x138;
const BTN_TR2        = 0x139;
const BTN_SELECT     = 0x13a;
const BTN_START      = 0x13b;
const BTN_MODE       = 0x13c;
const BTN_THUMBL     = 0x13d;
const BTN_THUMBR     = 0x13e;
const BTN_DPAD_UP    = 0x220;
const BTN_DPAD_DOWN  = 0x221;
const BTN_DPAD_LEFT  = 0x222;
const BTN_DPAD_RIGHT = 0x223;

// ── Linux evdev absolute axis codes (ABS_*) ───────────────────────────────────
const ABS_X  = 0x00;
const ABS_Y  = 0x01;
const ABS_Z  = 0x02;
const ABS_RX = 0x03;
const ABS_RY = 0x04;
const ABS_RZ = 0x05;

// ── Threshold for detecting gamepad input ─────────────────────────────────────
const BUTTON_THRESHOLD = 0.5;
const AXIS_THRESHOLD   = 0.5;

/**
 * Ordered wizard steps.  Each step describes one button or axis to detect.
 *
 * For `type: 'button'`, the wizard listens for a button value exceeding
 * BUTTON_THRESHOLD.  For `type: 'axis'`, it listens for an axis value whose
 * absolute value exceeds AXIS_THRESHOLD.
 *
 * `triggerAbsCode` is set on trigger buttons (LT/RT) to wire up the analog
 * axis alongside the digital key event, matching the standard layout behaviour.
 */
const WIZARD_STEPS = [
  // ── Face buttons ────────────────────────────────────────────────────────────
  { type: 'button', label: 'A / Cross',           hint: 'Bottom face button',        btnCode: BTN_SOUTH },
  { type: 'button', label: 'B / Circle',          hint: 'Right face button',         btnCode: BTN_EAST },
  { type: 'button', label: 'X / Square',          hint: 'Left face button',          btnCode: BTN_WEST },
  { type: 'button', label: 'Y / Triangle',        hint: 'Top face button',           btnCode: BTN_NORTH },
  // ── Shoulder buttons ────────────────────────────────────────────────────────
  { type: 'button', label: 'LB / L1',             hint: 'Left shoulder button',      btnCode: BTN_TL },
  { type: 'button', label: 'RB / R1',             hint: 'Right shoulder button',     btnCode: BTN_TR },
  // ── Triggers (digital + analog) ─────────────────────────────────────────────
  { type: 'button', label: 'LT / L2',             hint: 'Left trigger — press fully', btnCode: BTN_TL2, triggerAbsCode: ABS_Z },
  { type: 'button', label: 'RT / R2',             hint: 'Right trigger — press fully', btnCode: BTN_TR2, triggerAbsCode: ABS_RZ },
  // ── Menu buttons ────────────────────────────────────────────────────────────
  { type: 'button', label: 'Select / Back',       hint: 'Small left center button',  btnCode: BTN_SELECT },
  { type: 'button', label: 'Start / Menu',        hint: 'Small right center button', btnCode: BTN_START },
  // ── Stick clicks ────────────────────────────────────────────────────────────
  { type: 'button', label: 'L3 (Left Stick)',     hint: 'Click the left stick in',   btnCode: BTN_THUMBL },
  { type: 'button', label: 'R3 (Right Stick)',    hint: 'Click the right stick in',  btnCode: BTN_THUMBR },
  // ── D-pad ───────────────────────────────────────────────────────────────────
  { type: 'button', label: 'D-pad Up',            hint: 'Press D-pad Up',            btnCode: BTN_DPAD_UP },
  { type: 'button', label: 'D-pad Down',          hint: 'Press D-pad Down',          btnCode: BTN_DPAD_DOWN },
  { type: 'button', label: 'D-pad Left',          hint: 'Press D-pad Left',          btnCode: BTN_DPAD_LEFT },
  { type: 'button', label: 'D-pad Right',         hint: 'Press D-pad Right',         btnCode: BTN_DPAD_RIGHT },
  // ── Guide / Home (optional) ─────────────────────────────────────────────────
  { type: 'button', label: 'Guide / Home',        hint: 'Center logo button (optional)', btnCode: BTN_MODE },
  // ── Analog axes ─────────────────────────────────────────────────────────────
  { type: 'axis',   label: 'Left Stick — X axis', hint: 'Move the left stick left or right', absCode: ABS_X },
  { type: 'axis',   label: 'Left Stick — Y axis', hint: 'Move the left stick up or down',    absCode: ABS_Y },
  { type: 'axis',   label: 'Right Stick — X axis',hint: 'Move the right stick left or right',absCode: ABS_RX },
  { type: 'axis',   label: 'Right Stick — Y axis',hint: 'Move the right stick up or down',   absCode: ABS_RY },
];

export class GamepadMapper {
  #gamepad;       // Gamepad object at the time the wizard was opened
  #onComplete;    // (declaration) => void
  #stepIndex = 0;
  #rafHandle = null;

  // Result arrays indexed by raw browser index.
  // Entries remain null for skipped or unmapped slots.
  #buttonResult = [];
  #axisResult   = [];

  // Track previous gamepad state to detect new presses/movements.
  #prevButtons = [];
  #prevAxes    = [];

  // DOM refs
  #overlay  = null;
  #modal    = null;

  /**
   * @param {Gamepad} gamepad - The non-standard gamepad to map.
   * @param {(declaration: {buttons: Array, axes: Array}) => void} onComplete
   */
  constructor(gamepad, onComplete) {
    this.#gamepad    = gamepad;
    this.#onComplete = onComplete;
  }

  /** Open the wizard modal and start detection. */
  show() {
    this.#stepIndex    = 0;
    this.#buttonResult = new Array(this.#gamepad.buttons.length).fill(null);
    this.#axisResult   = new Array(this.#gamepad.axes.length).fill(null);

    // Snapshot current state so only *new* input triggers detection.
    const snap = this.#currentSnapshot();
    this.#prevButtons = snap.buttons;
    this.#prevAxes    = snap.axes;

    this.#buildDOM();
    this.#renderStep();
    this.#startPoll();
  }

  /** Close and clean up without completing. */
  cancel() {
    this.#stopPoll();
    this.#overlay?.remove();
    this.#overlay = null;
  }

  // ── Private ───────────────────────────────────────────────────────────────

  #buildDOM() {
    this.#overlay = document.createElement('div');
    this.#overlay.className = 'gm-overlay';

    this.#modal = document.createElement('div');
    this.#modal.className = 'gm-modal';
    this.#modal.innerHTML = `
      <div class="gm-header">
        <span class="gm-title">Map Controller</span>
        <button class="gm-cancel-btn" title="Cancel mapping">✕</button>
      </div>
      <div class="gm-controller-name"></div>
      <div class="gm-progress-bar"><div class="gm-progress-fill"></div></div>
      <div class="gm-step-area">
        <div class="gm-step-icon"></div>
        <div class="gm-step-label"></div>
        <div class="gm-step-hint"></div>
        <div class="gm-step-status"></div>
      </div>
      <div class="gm-actions">
        <button class="gm-skip-btn">Skip</button>
      </div>
    `;

    this.#modal.querySelector('.gm-controller-name').textContent = this.#gamepad.id;
    this.#modal.querySelector('.gm-cancel-btn').addEventListener('click', () => this.cancel());
    this.#modal.querySelector('.gm-skip-btn').addEventListener('click', () => this.#advance());

    this.#overlay.appendChild(this.#modal);
    document.body.appendChild(this.#overlay);
  }

  #renderStep() {
    if (this.#stepIndex >= WIZARD_STEPS.length) {
      this.#finish();
      return;
    }
    const step     = WIZARD_STEPS[this.#stepIndex];
    const total    = WIZARD_STEPS.length;
    const progress = this.#stepIndex / total * 100;

    this.#modal.querySelector('.gm-progress-fill').style.width = `${progress}%`;
    this.#modal.querySelector('.gm-step-icon').textContent  = step.type === 'button' ? '🔘' : '🕹️';
    this.#modal.querySelector('.gm-step-label').textContent = step.label;
    this.#modal.querySelector('.gm-step-hint').textContent  = step.hint;
    this.#modal.querySelector('.gm-step-status').textContent = `Step ${this.#stepIndex + 1} of ${total}`;
  }

  /** Record a detected input at the current step and advance. */
  #recordAndAdvance(rawIndex) {
    const step = WIZARD_STEPS[this.#stepIndex];
    if (step.type === 'button') {
      this.#buttonResult[rawIndex] = {
        btn_code:         step.btnCode,
        trigger_abs_code: step.triggerAbsCode ?? null,
      };
    } else {
      this.#axisResult[rawIndex] = { abs_code: step.absCode };
    }
    this.#flashDetected();
    // Brief pause so the user sees feedback, then advance.
    setTimeout(() => this.#advance(), 400);
  }

  /** Move to the next step without recording anything (skip). */
  #advance() {
    this.#stepIndex++;
    // Reset prev state so the next step starts fresh.
    const snap = this.#currentSnapshot();
    this.#prevButtons = snap.buttons;
    this.#prevAxes    = snap.axes;
    this.#renderStep();
  }

  /** Flash the step area green to confirm detection. */
  #flashDetected() {
    const area = this.#modal.querySelector('.gm-step-area');
    area.classList.add('gm-detected');
    this.#modal.querySelector('.gm-step-status').textContent = '✓ Detected!';
    setTimeout(() => area.classList.remove('gm-detected'), 350);
  }

  #finish() {
    this.#stopPoll();
    this.#overlay?.remove();
    this.#overlay = null;
    this.#onComplete({
      buttons: this.#buttonResult,
      axes:    this.#axisResult,
    });
  }

  // ── Detection loop ────────────────────────────────────────────────────────

  #startPoll() {
    const poll = () => {
      this.#detectInput();
      this.#rafHandle = requestAnimationFrame(poll);
    };
    this.#rafHandle = requestAnimationFrame(poll);
  }

  #stopPoll() {
    if (this.#rafHandle !== null) {
      cancelAnimationFrame(this.#rafHandle);
      this.#rafHandle = null;
    }
  }

  /**
   * Read the current gamepad state from the browser and compare with the
   * previous snapshot to detect new button presses or axis movements.
   */
  #detectInput() {
    if (this.#stepIndex >= WIZARD_STEPS.length) return;
    const step = WIZARD_STEPS[this.#stepIndex];

    // Re-query the live gamepad object each frame (browser snapshots it).
    const gamepads = navigator.getGamepads();
    const gp = gamepads[this.#gamepad.index];
    if (!gp) return;

    if (step.type === 'button') {
      for (let i = 0; i < gp.buttons.length; i++) {
        const cur  = gp.buttons[i].value;
        const prev = this.#prevButtons[i] ?? 0;
        if (cur >= BUTTON_THRESHOLD && prev < BUTTON_THRESHOLD) {
          this.#prevButtons[i] = cur;
          this.#recordAndAdvance(i);
          return;
        }
        this.#prevButtons[i] = cur;
      }
    } else {
      for (let i = 0; i < gp.axes.length; i++) {
        const cur  = gp.axes[i];
        const prev = this.#prevAxes[i] ?? 0;
        if (Math.abs(cur) >= AXIS_THRESHOLD && Math.abs(prev) < AXIS_THRESHOLD) {
          this.#prevAxes[i] = cur;
          this.#recordAndAdvance(i);
          return;
        }
        this.#prevAxes[i] = cur;
      }
    }
  }

  #currentSnapshot() {
    const gamepads = navigator.getGamepads();
    const gp = gamepads[this.#gamepad.index] ?? this.#gamepad;
    return {
      buttons: Array.from(gp.buttons, b => b.value),
      axes:    Array.from(gp.axes),
    };
  }
}
