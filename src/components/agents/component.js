  const openAgents=()=>document.querySelector("#agent-dialog").showModal();
  const focusAgent=id=>{fetch(`/components/agents/focus/${encodeURIComponent(id)}`,{method:"POST"});closeDialog(document.querySelector("#agent-dialog"))};
