import { css } from '../macro' with {type: 'macro'};

export default function Home() {
  return (
    <main>
      <h1 className={css('font-family: system-ui; color: blue')}>Hello world!</h1>
    </main>
  );
}
