document.addEventListener("pointerdown", event => {
  const monitor = event.target.closest("#monitors .monitor-view");
  if (!monitor) return;
  const startX = event.clientX;
  monitor.setPointerCapture(event.pointerId);
  monitor.onpointerup = released => {
    const delta = released.clientX - startX;
    const base = Number(monitor.dataset.base);
    const active = Number(monitor.dataset.active);
    const steps = Math.min(8, Math.floor(Math.abs(delta) / 25));
    let target = active;
    if (steps) {
      target = Math.max(base, Math.min(base + 8, active + (delta < 0 ? steps : -steps)));
    }
    post(`/components/focus/workspace/${target}`);
  };
});
