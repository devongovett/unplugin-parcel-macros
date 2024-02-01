import './style.css'
import { readFileSync } from 'fs' with {type: 'macro'};
import { setupCounter } from './counter.js'
import { css } from "./macro" with {type:'macro'};

document.querySelector('#app').innerHTML = `
  <div>
    <a href="https://vitejs.dev" target="_blank">
      ${readFileSync('public/vite.svg', 'utf8')}
    </a>
    <a href="https://developer.mozilla.org/en-US/docs/Web/JavaScript" target="_blank">
      ${readFileSync('javascript.svg', 'utf8')}
    </a>
    <h1>Hello Vite!</h1>
    <div class="card">
      <button id="counter" type="button"></button>
    </div>
    <p class="${css('color: red')}">
      Click on the Vite logo to learn more
    </p>
  </div>
`

setupCounter(document.querySelector('#counter'))
