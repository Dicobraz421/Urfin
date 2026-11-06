// Обзор TSX для дифференциального теста подсветки кода (ADR-0024 §8).
import React, { useState, useEffect } from "react";
import type { Foo } from "./foo";

export interface Props<T> {
  items: T[];
  title?: string;
  render: (item: T) => JSX.Element;
}

type Pair<A, B> = { a: A; b: B };

enum Kind {
  One,
  Two = 2,
}

const identity = <T,>(x: T): T => x;

function pick<T extends object, K extends keyof T>(o: T, k: K): T[K] {
  return o[k];
}

export function List<T>({ items, title, render }: Props<T>) {
  const [open, setOpen] = useState(false);
  const label = title ?? "без имени";
  const re = /[{}'"`]+/g;
  const half = items.length / 2 / 1;
  const text = `шаблон ${label} и ${items.map((x) => `вложенный ${String(x)}`).join(",")}`;

  useEffect(() => {
    if (open) {
      console.log("открыт");
      console.log("ещё");
    }
    return () => {
      console.log("закрыт");
    };
  }, [open]);

  if (!items.length) {
    return <p className="empty">it's empty: don't panic</p>;
  }

  return (
    <div className="list" onClick={() => setOpen(!open)}>
      <h1>{label}</h1>
      {/* комментарий */}
      <ul>
        {items.map((item, i) => (
          <li key={i} data-x="a'b">
            {render(item)} текст с "кавычками" и 'апострофом
          </li>
        ))}
      </ul>
      {open ? <span>да</span> : <span>нет</span>}
      <>
        <br />
        <input value={text} disabled />
      </>
    </div>
  );
}

export class Box<T> extends React.Component<{ value: T }> {
  private count = 0;
  static defaultProps = {
    value: null,
  };

  get double() {
    return this.count * 2;
  }

  render() {
    const { value } = this.props;
    return <div>{String(value)}</div>;
  }
}

export default function App() {
  const data = [
    1,
    2,
    3,
  ];
  const conf = {
    a: 1,
    b: "x",
    c() {
      return 1;
    },
  };
  switch (data.length) {
    case 1:
      conf.a = 2;
      conf.a = 3;
      break;
    default:
      conf.a = 4;
  }
  for (const x of data) {
    console.log(x);
  }
  return <List items={data} render={(x) => <b>{x}</b>} />;
}

const pick = <T extends object>(x: T) => {
  return x;
};

const tail = <T,>(x: T) => {
  return x;
};
