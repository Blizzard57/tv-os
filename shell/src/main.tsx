import React from 'react';
import ReactDOM from 'react-dom/client';
// Roboto — the typeface the Android/Google-TV system UI uses. Static weight
// files (not a variable font): Linux Chromium's FreeType path renders
// interpolated weights badly.
import '@fontsource/roboto/300.css';
import '@fontsource/roboto/400.css';
import '@fontsource/roboto/500.css';
import '@fontsource/roboto/700.css';
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
