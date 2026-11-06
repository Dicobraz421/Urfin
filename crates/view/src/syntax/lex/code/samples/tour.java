// Обзор языка для дифференциального теста подсветки кода (ADR-0024 §8).
package com.example.app;

import java.util.*;
import java.util.function.Function;
import static java.lang.Math.max;

/**
 * Документация класса.
 */
@Deprecated
public abstract class Tour<T extends Comparable<T>> implements Iterable<T>, Runnable {
    private static final int LIMIT = 1_000;
    protected final List<T> items = new ArrayList<>();
    public int[] table = {
        1,
        2,
        3,
    };
    String[][] names = new String[][] {
        {"a", "b"},
        {"c", "d"},
    };
    static {
        System.out.println("static init");
    }
    {
        items.clear();
    }

    public Tour() {
        this(10);
    }

    protected Tour(int size) {
        super();
        for (int i = 0; i < size; i++) {
            items.add(null);
        }
    }

    @Override
    public void run() {
        int x = 1, y = 2;
        long z = 0xFFL + 017 + 0b11;
        double d = 1.5e3, e = .5f;
        char c = '\'', q = '"', u = 'A';
        String s = "строка с \"кавычками\" и // не комментарий";
        String block = """
            text block with "quotes"
              and \""" escaped
            """;
        if (x > y) {
            y = x;
        } else if (x < y) {
            x = y;
        }
        // комментарий
        else {
            x = 0;
        }
        while (x < 10) x++;
        do {
            x--;
        } while (x > 0);
        for (T t : items) {
            System.out.println(t);
        }
        switch (x) {
            case 1:
                y = 2;
                y = 3;
                break;
            case 2: {
                y = 4;
                y = 5;
            }
            default:
                y = 6;
        }
        int r = switch (y) {
            case 1 -> 10;
            case 2 -> {
                yield 20;
            }
            default -> 30;
        };
        try (var in = new java.io.StringReader(s)) {
            in.read();
        } catch (java.io.IOException | RuntimeException ex) {
            throw new IllegalStateException(ex);
        } finally {
            y = 0;
        }
        synchronized (this) {
            x = 1;
        }
        Runnable a = () -> {
            int v = 1;
            int w = 2;
            System.out.println(v + w);
        };
        Function<Integer, Integer> f = (Integer n) -> n + 1;
        Object o = new Object() {
            int hidden = 1;

            @Override
            public String toString() {
                return "anon" + hidden;
            }
        };
        items.forEach(t -> {
            System.out.println(t);
            System.out.println(t);
        });
        new Thread(new Runnable() {
            @Override
            public void run() {
                System.out.println("x");
                System.out.println("y");
            }
        }).start();
        label:
        for (;;) {
            break label;
        }
        assert x > 0 : "message";
    }

    public abstract <R> R map(Function<? super T, ? extends R> f) throws Exception;

    @SuppressWarnings({
        "unchecked",
        "rawtypes",
    })
    public static <A extends Comparable<A>> Tour<A> of(A... xs) {
        return null;
    }

    public Iterator<T> iterator() {
        return items.iterator();
    }

    interface Shape permits Circle, Square {
        double area();

        default String name() {
            return "shape";
        }
    }

    record Circle(double r) implements Shape {
        Circle {
            if (r < 0) {
                throw new IllegalArgumentException();
            }
        }

        public double area() {
            return Math.PI * r * r;
        }
    }

    record Square(double a) implements Shape {
        public double area() {
            return a * a;
        }
    }

    enum Color {
        RED,
        GREEN("g") {
            @Override
            String code() {
                return "gg";
            }
        },
        BLUE;

        private final String id;

        Color() {
            this.id = "";
        }

        Color(String id) {
            this.id = id;
        }

        String code() {
            return id;
        }
    }

    @interface Marker {
        String value() default "x";

        int[] nums() default {1, 2};
    }

    static class Node<K, V> {
        K key;
        V value;
        Node<K, V> next;

        Node(K key, V value) {
            this.key = key;
            this.value = value;
        }
    }
}

class Second {
    int f(int a) {
        return a;
    }
}

interface Third {
    int g();
}

class Extra {
    int[] table = new int[] {
        1,
        2,
    };

    int dims()[] {
        return null;
    }

    int pick(int k) {
        return switch (choose(() -> {
            return k;
        })) {
            case 1 -> {
                yield -1;
            }
            default -> 0;
        };
    }

    void guarded() {
        try (var r = open(() -> {
            return 1;
        })) {
            run();
        } catch (RuntimeException e) {
            run();
        }
        synchronized (lock(() -> {
            return 2;
        })) {
            run();
        }
    }
}
