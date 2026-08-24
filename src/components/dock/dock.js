const post = path => fetch(path, {method: "POST"});
  const closeDialog = dialog => {if(!dialog.open||dialog.classList.contains("closing"))return;if(dialog.id==='media-dialog')post('/components/media/visualizer/false');dialog.classList.add("closing");setTimeout(()=>{dialog.close();dialog.classList.remove("closing")},240)};
  document.addEventListener("htmx:wsClose",()=>console.warn("Dock WebSocket closed; HTMX will reconnect"));
