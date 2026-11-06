// Обзор языка для дифференциального теста подсветки кода (ADR-0024 §8).
using System;
using System.Collections.Generic;
using static System.Math;

#nullable enable
#region Основное

namespace Example.App
{
    /// <summary>Документация.</summary>
    [Serializable]
    [Obsolete("old")]
    public abstract class Tour<T> : IDisposable where T : class, new()
    {
        private const int Limit = 1_000;
        private readonly List<T> items = new();
        public int[] Table = {
            1,
            2,
            3,
        };
        public string Name { get; set; } = "x";
        public int Count
        {
            get
            {
                var a = items.Count;
                var b = a + 1;
                return b;
            }
            set
            {
                Console.WriteLine(value);
                Console.WriteLine(value);
            }
        }
        public event EventHandler? Changed
        {
            add { }
            remove { }
        }

        public Tour() : this(10)
        {
        }

        protected Tour(int size)
        {
            for (var i = 0; i < size; i++)
            {
                items.Add(new T());
            }
        }

        public void Run()
        {
            int x = 1, y = 2;
            long z = 0xFFL + 0b11;
            double d = 1.5e3;
            char c = '\'', q = '"';
            string s = "строка \"с кавычками\" // не комментарий";
            string v = @"дословная ""кавычка""
    на две строки";
            string i = $"x = {x}, y = {y:N2}, {{скобки}}, {(x > 0 ? "a" : "b")}";
            string vi = $@"путь {x} ""кавычка""
    вторая {y}";
            string r = """
                сырая "строка"
                  с отступом
                """;
            string ri = $"""
                значение {x}
                """;
            if (x > y)
            {
                y = x;
            }
            else if (x < y)
            {
                x = y;
            }
            // комментарий
            else
            {
                x = 0;
            }
            while (x < 10) x++;
            do
            {
                x--;
            } while (x > 0);
            foreach (var t in items)
            {
                Console.WriteLine(t);
            }
            switch (x)
            {
                case 1:
                    y = 2;
                    y = 3;
                    break;
                case 2:
                {
                    y = 4;
                    break;
                }
                default:
                    y = 6;
                    break;
            }
            var w = x switch
            {
                1 => "a",
                2 => "b",
                _ => "c",
            };
            try
            {
                y = 1;
            }
            catch (Exception ex) when (ex is not null)
            {
                throw;
            }
            finally
            {
                y = 0;
            }
            using (var m = new System.IO.MemoryStream())
            {
                m.WriteByte(1);
            }
            lock (items)
            {
                x = 1;
            }
            Action a = () =>
            {
                var p = 1;
                var o = 2;
                Console.WriteLine(p + o);
            };
            Func<int, int> f = n => n + 1;
            var obj = new Dictionary<string, int>
            {
                { "a", 1 },
                { "b", 2 },
            };
            var anon = new { A = 1, B = "x" };
            var list = new List<int> { 1, 2, 3 };
            var q2 = from e in list
                     where e > 1
                     select e * 2;
            items.ForEach(t =>
            {
                Console.WriteLine(t);
                Console.WriteLine(t);
            });
#if DEBUG
            Console.WriteLine("debug");
#else
            Console.WriteLine("release");
#endif
            unsafe
            {
                int* ptr = null;
            }
            checked
            {
                x = x + 1;
            }
        }

        public abstract TR Map<TR>(Func<T, TR> f);

        public void Dispose() => items.Clear();

        public static Tour<T> operator +(Tour<T> a, Tour<T> b) => a;
    }

    public interface IShape
    {
        double Area();

        string Name => "shape";
    }

    public record Circle(double R) : IShape
    {
        public double Area() => Math.PI * R * R;
    }

    public struct Point
    {
        public int X;
        public int Y;

        public Point(int x, int y)
        {
            X = x;
            Y = y;
        }
    }

    public enum Color
    {
        Red,
        Green = 2,
        Blue,
    }

    public delegate int Handler(int a);

    internal static class Extensions
    {
        public static int Twice(this int a)
        {
            return a * 2;
        }
    }
}

#endregion

namespace Example.Other;

public class Second
{
    public int F(int a)
    {
        return a;
    }
}

class Extra
{
    void Query()
    {
        var q = from x in xs
                join y in ys on new
                {
                    x.A,
                }
                equals new
                {
                    y.A,
                }
                select new
                {
                    x.A,
                }
                into r
                select r;
        var z = q is
        {
            Length: 1,
        }
            or
        {
            Length: 2,
        };
        if (z)
        {
            Run();
        }
#pragma warning disable 618
        else
        {
            Run();
        }
#pragma warning restore 618
    }
}
