let awakeUntil = Number(localStorage.getItem("raylander-awake-until") || 0);
let awakeTimer;

globalThis.renderAwake = () => {
  const button = document.querySelector("#awake-control");
  if (!button) return;
  const left = Math.max(0, awakeUntil - Date.now());
  button.classList.toggle("active", left > 0);
  button.querySelector("small").textContent = left ? `${Math.ceil(left / 60000)}m` : "";
  clearTimeout(awakeTimer);
  if (left) awakeTimer = setTimeout(globalThis.renderAwake, Math.min(60000, left + 50));
  else localStorage.removeItem("raylander-awake-until");
};

globalThis.keepAwake = () => {
  const active = awakeUntil > Date.now();
  awakeUntil = active ? 0 : Date.now() + 60 * 60 * 1000;
  if (active) {
    localStorage.removeItem("raylander-awake-until");
    post("/components/connectivity/awake/false");
  } else {
    localStorage.setItem("raylander-awake-until", awakeUntil);
    post("/components/connectivity/awake/true");
  }
  globalThis.renderAwake();
};

addEventListener("DOMContentLoaded", () => {
  globalThis.renderAwake();
  if (awakeUntil > Date.now()) post("/components/connectivity/awake/true");
});

globalThis.bluetoothCommand = (action, button) => {
  button.disabled = true;
  fetch(`/components/connectivity/bluetooth/${action}`, {method: "POST"})
    .finally(() => setTimeout(() => { button.disabled = false; }, 600));
};

globalThis.bluetoothAction = (address, action, button) => {
  button.disabled = true;
  fetch(`/components/connectivity/bluetooth/device/${encodeURIComponent(address)}/${action}`, {method: "POST"})
    .finally(() => setTimeout(() => { button.disabled = false; }, 800));
};
