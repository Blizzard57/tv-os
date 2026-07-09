import React from 'react';
import ReactDOM from 'react-dom/client';
// Static weight files, not the variable font: Linux Chromium's FreeType
// path renders interpolated heavy weights (700–800 display titles) badly.
import '@fontsource/inter/400.css';
import '@fontsource/inter/500.css';
import '@fontsource/inter/600.css';
import '@fontsource/inter/700.css';
import '@fontsource/inter/800.css';
import App from './App';
import './styles.css';

const rootEl = document.getElementById('root');
if (!rootEl) {
  throw new Error('TV OS shell: #root element missing from index.html — cannot mount.');
}

ReactDOM.createRoot(rootEl).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
