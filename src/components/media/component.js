  const openMedia=()=>{document.querySelector('#media-dialog').showModal();post('/components/media/visualizer/true')};
  let spectrumFrame;
  const renderSpectrum=()=>{cancelAnimationFrame(spectrumFrame);spectrumFrame=requestAnimationFrame(()=>{const data=document.querySelector('#spectrum-data');const bars=document.querySelectorAll('#media-visualizer>i');if(!data||!bars.length)return;data.dataset.levels.split(',').forEach((level,index)=>{if(bars[index])bars[index].style.height=`${Math.max(3,+level)}%`})})};
  document.addEventListener('htmx:oobAfterSwap',renderSpectrum);
  addEventListener('DOMContentLoaded',()=>document.querySelector('#media-dialog').addEventListener('close',()=>post('/components/media/visualizer/false')));
  addEventListener('pagehide',()=>navigator.sendBeacon('/components/media/visualizer/false'));
