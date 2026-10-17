  let volumeTimer, pendingVolume, outputBusy = false, lastVolumeEdit = 0;
  let volumeRequest = Promise.resolve();
  const sendVolume = () => {
    const value = pendingVolume;
    pendingVolume = undefined;
    volumeTimer = undefined;
    if (value === undefined) return volumeRequest;
    volumeRequest = volumeRequest.then(() => fetch(`/components/controls/volume/${value}`, {
      method: "POST", signal: AbortSignal.timeout(10000)
    }))
      .catch(error => console.warn("Volume update failed", error));
    return volumeRequest;
  };
  const setVolume = value => {
    if (outputBusy) return;
    lastVolumeEdit = Date.now();
    const percent = Math.max(0, Math.min(100, Math.round(value)));
    document.querySelector("#volume-fill").style.height = `${percent}%`;
    document.querySelector("#volume-dialog-fill").style.height = `${percent}%`;
    document.querySelector("#volume-dialog-value").textContent = `${percent}%`;
    pendingVolume=percent;
    if(!volumeTimer)volumeTimer=setTimeout(sendVolume,32);
  };
  const syncDisplayPower=()=>{const state=document.querySelector("#display-power")?.dataset.on!=="false";globalThis.RaylanderAndroid?.setDisplayEnabled(state)};
const bindControls = () => {
    const gauge=document.querySelector("#volume-gauge"),brightnessGauge=document.querySelector("#brightness-gauge");let startY=0,moved=false,last=-1,pendingBrightness=0;
    gauge.onpointerdown=e=>{e.preventDefault();startY=e.clientY;moved=false;gauge.setPointerCapture(e.pointerId)};
    gauge.onpointermove=e=>{if(!gauge.hasPointerCapture(e.pointerId))return;if(Math.abs(e.clientY-startY)>5)moved=true;const r=gauge.getBoundingClientRect(),value=(r.bottom-e.clientY)/r.height*100;if(moved&&Math.round(value)!==last){last=Math.round(value);setVolume(value)}};
    brightnessGauge.onpointerdown=e=>{e.preventDefault();startY=e.clientY;moved=false;brightnessGauge.setPointerCapture(e.pointerId)};
    brightnessGauge.onpointermove=e=>{if(!brightnessGauge.hasPointerCapture(e.pointerId))return;if(Math.abs(e.clientY-startY)>5)moved=true;const r=brightnessGauge.getBoundingClientRect();pendingBrightness=Math.max(0,Math.min(100,Math.round((r.bottom-e.clientY)/r.height*100)));document.querySelector("#brightness-fill").style.height=`${pendingBrightness}%`;document.querySelector("#brightness-dialog-fill").style.height=`${pendingBrightness}%`;document.querySelector("#brightness-dialog-value").textContent=`${pendingBrightness}%`};
    const dialog=document.querySelector("#volume-dialog"),largeGauge=document.querySelector("#volume-dialog-gauge"),largeBrightness=document.querySelector("#brightness-dialog-gauge");let closeTimer;
    let outputError = false, outputPoll, outputController, selectingTarget;
    const keepDialogOpen = () => {
      clearTimeout(closeTimer);
      if (!outputBusy && !outputError) closeTimer = setTimeout(() => closeDialog(dialog), 5000);
    };
    const outputPanel = document.querySelector("#audio-output-panel");
    const outputList = document.querySelector("#audio-output-list");
    const outputStatus = document.querySelector("#audio-output-status");
    const targetKey = target => JSON.stringify(target);
    const showOutputMessage = (message, error = false) => {
      outputError = error;
      outputStatus.textContent = message || "";
      outputStatus.classList.toggle("audio-output-error", error);
      keepDialogOpen();
    };
    const syncVolume = () => {
      if (outputBusy || volumeTimer || Date.now() - lastVolumeEdit < 1000) return;
      const height = document.querySelector("#volume-fill").style.height || "0%";
      if (document.querySelector("#volume-dialog-fill").style.height !== height) {
        document.querySelector("#volume-dialog-fill").style.height = height;
        document.querySelector("#volume-dialog-value").textContent = `${Math.round(parseFloat(height))}%`;
      }
    };
    new MutationObserver(syncVolume).observe(gauge, {childList: true, subtree: true, attributes: true, attributeFilter: ["style"]});
    const renderOutputs = state => {
      outputPanel.hidden = !state.supported;
      dialog.querySelector(".volume-dialog-panel").classList.toggle("has-audio-outputs", state.supported);
      if (!state.supported) return;
      // Reconcile buttons rather than replacing focused or pressed rows every two seconds.
      const keys = new Set();
      for (const output of state.outputs) {
        const key = targetKey(output.target);
        keys.add(key);
        let button = [...outputList.children].find(row => row.dataset.target === key);
        if (!button) {
          button = document.createElement("button");
          button.type = "button";
          button.dataset.target = key;
          button.append(document.createElement("span"), document.createElement("small"));
          button.onclick = () => selectOutput(output.target);
          outputList.append(button);
        }
        button.firstChild.textContent = output.name;
        const pending = outputBusy && targetKey(selectingTarget) === key;
        button.lastChild.textContent = pending ? (output.connected ? "Switching…" : "Connecting…")
          : output.selected ? "Selected" : output.connected ? "Connected" : "Not connected";
        button.setAttribute("aria-pressed", String(output.selected));
        button.disabled = outputBusy;
      }
      for (const button of [...outputList.children]) {
        if (!keys.has(button.dataset.target)) button.remove();
      }
      if (!outputBusy && !outputError) outputStatus.textContent = state.warning || (keys.size ? "" : "No audio outputs available");
      if (!outputBusy && state.volume !== null && Date.now() - lastVolumeEdit >= 1000) {
        document.querySelector("#volume-fill").style.height = `${state.volume}%`;
        syncVolume();
      }
    };
    const refreshOutputs = async () => {
      if (!dialog.open || outputController) return;
      const controller = new AbortController();
      outputController = controller;
      let timedOut = false;
      const requestTimeout = setTimeout(() => { timedOut = true; controller.abort(); }, 20000);
      try {
        const response = await fetch("/components/controls/audio/outputs", {signal: controller.signal});
        const state = await response.json();
        if (!response.ok) throw new Error(state.error || "Unable to load audio outputs");
        if (dialog.open && !controller.signal.aborted) renderOutputs(state);
      } catch (error) {
        if (dialog.open && (!controller.signal.aborted || timedOut)) {
          outputPanel.hidden = false;
          dialog.querySelector(".volume-dialog-panel").classList.add("has-audio-outputs");
          showOutputMessage(timedOut ? "Loading audio outputs timed out. Please retry." : error.message, true);
        }
      } finally {
        clearTimeout(requestTimeout);
        if (outputController === controller) outputController = undefined;
      }
    };
    const selectOutput = async target => {
      if (outputBusy) return;
      outputBusy = true;
      selectingTarget = target;
      outputError = false;
      clearTimeout(closeTimer);
      clearTimeout(volumeTimer);
      volumeTimer = undefined;
      outputList.setAttribute("aria-busy", "true");
      largeGauge.setAttribute("aria-disabled", "true");
      for (const button of outputList.children) {
        button.disabled = true;
        if (button.dataset.target === targetKey(target)) button.lastChild.textContent = target.kind === "bluetooth" ? "Connecting…" : "Switching…";
      }
      showOutputMessage(target.kind === "bluetooth" ? "Connecting…" : "Switching…");
      let message = "", failed = false;
      try {
        await sendVolume();
        const response = await fetch("/components/controls/audio/output", {
          method: "POST", headers: {"Content-Type": "application/json"}, body: JSON.stringify(target),
          signal: AbortSignal.timeout(65000)
        });
        const result = await response.json();
        if (!response.ok) throw new Error(result.error || "Unable to select output");
        message = result.warning || "";
        failed = Boolean(result.warning);
      } catch (error) {
        message = error.message;
        failed = true;
      } finally {
        outputBusy = false;
        selectingTarget = undefined;
        lastVolumeEdit = 0;
        outputList.setAttribute("aria-busy", "false");
        largeGauge.removeAttribute("aria-disabled");
        for (const button of outputList.children) button.disabled = false;
        outputController?.abort();
        outputController = undefined;
        showOutputMessage(message, failed);
        await refreshOutputs();
        keepDialogOpen();
      }
    };
    outputPanel.addEventListener("pointerdown", keepDialogOpen);
    outputPanel.addEventListener("keydown", keepDialogOpen);
    outputPanel.addEventListener("wheel", keepDialogOpen, {passive: true});
    dialog.addEventListener("close", () => {
      clearTimeout(closeTimer);
      clearInterval(outputPoll);
      outputController?.abort();
      outputController = undefined;
    });
    const openDialog=()=>{dialog.dataset.ignoreClickUntil=Date.now()+350;dialog.showModal();outputError=false;clearInterval(outputPoll);refreshOutputs();outputPoll=setInterval(refreshOutputs,2000);for(const name of ["volume","brightness"]){const height=parseFloat(document.querySelector(`#${name}-fill`).style.height)||0;document.querySelector(`#${name}-dialog-fill`).style.height=`${height}%`;document.querySelector(`#${name}-dialog-value`).textContent=`${Math.round(height)}%`}keepDialogOpen()};
    gauge.onpointerup=()=>{if(!moved)openDialog()};brightnessGauge.onpointerup=()=>{if(moved)post(`/components/controls/brightness/${pendingBrightness}`);else openDialog()};
    const updateLargeGauge=e=>{const r=largeGauge.getBoundingClientRect();setVolume((r.bottom-e.clientY)/r.height*100);keepDialogOpen()};
    largeGauge.onpointerdown=e=>{e.preventDefault();largeGauge.setPointerCapture(e.pointerId)};
    largeGauge.onpointermove=e=>{if(largeGauge.hasPointerCapture(e.pointerId))updateLargeGauge(e)};
    largeGauge.onwheel=e=>{e.preventDefault();const current=parseFloat(document.querySelector("#volume-dialog-fill").style.height)||0;setVolume(current-Math.sign(e.deltaY)*2);keepDialogOpen()};
    const updateLargeBrightness=e=>{const r=largeBrightness.getBoundingClientRect();pendingBrightness=Math.max(0,Math.min(100,Math.round((r.bottom-e.clientY)/r.height*100)));document.querySelector("#brightness-dialog-fill").style.height=`${pendingBrightness}%`;document.querySelector("#brightness-fill").style.height=`${pendingBrightness}%`;document.querySelector("#brightness-dialog-value").textContent=`${pendingBrightness}%`;keepDialogOpen()};
    largeBrightness.onpointerdown=e=>{e.preventDefault();largeBrightness.setPointerCapture(e.pointerId)};largeBrightness.onpointermove=e=>{if(largeBrightness.hasPointerCapture(e.pointerId))updateLargeBrightness(e)};largeBrightness.onpointerup=()=>post(`/components/controls/brightness/${pendingBrightness}`);
    dialog.onclick=e=>{if(e.target.id==="volume-dialog"&&Date.now()>Number(dialog.dataset.ignoreClickUntil||0)){clearTimeout(closeTimer);closeDialog(dialog)}};
};
document.addEventListener("DOMContentLoaded",()=>{syncDisplayPower();bindControls();new MutationObserver(syncDisplayPower).observe(document.body,{childList:true,subtree:true,attributes:true,attributeFilter:["data-on"]})});
