import { css } from '../macro' with {type: 'macro'};

const color = 'blue' as const;

export default function Home() {
  return (
    <main>
      <h1 className={css('font-family: system-ui; color: ' + color)}>Hello world!</h1>
    </main>
  );
}
