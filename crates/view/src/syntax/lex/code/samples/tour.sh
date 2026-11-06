#!/usr/bin/env bash
# Обзор языка: границы на каждом кандидате, окно = полный разбор.
set -euo pipefail

readonly NAME="${1:-мир}"
declare -a list=(один "два три" 'четыре')
declare -A map=([a]=1 [b]=2)
count=0
export PATH="$HOME/bin:$PATH"

usage() {
    cat <<EOF
Использование: $0 [файл]
  текст с $NAME и $(date +%F)
EOF
}

log () {
    local level=$1; shift
    printf '%s: %s\n' "$level" "$*" >&2
}

function cleanup {
    rm -f "${tmp:-}" 2>/dev/null || true
}

function with_parens() {
    echo "$#" "$@" "${#list[@]}" "${list[*]}"
}

trap cleanup EXIT

if [ $# -gt 2 ]; then
    usage
    exit 1
elif [[ "$NAME" == *.txt && -f $NAME ]]; then
    echo "текстовый"
else
    echo 'прочее'
fi

if command -v git >/dev/null 2>&1
then
    echo есть
fi

for f in *.txt "a b.txt"; do
    [ -e "$f" ] || continue
    wc -l "$f"
done

for ((i = 0; i < 3; i++)); do
    echo $((i * 2 << 1))
done

while read -r line; do
    count=$((count + 1))
    echo "$line"
done < <(ls -1)

until false; do
    break
done

case "$NAME" in
    a|b)
        echo ab
        ;;
    *.sh) echo скрипт ;;
    (c) echo с;;
    *)
        echo "$(case x in y) echo z;; esac)"
        ;;
esac

{
    echo группа
    echo в файл
} > /dev/null

(
    cd /tmp
    pwd
)

result=$(
    cat <<'RAW' | tr a b
сырой $текст `без` подстановок
RAW
)

text="многострочная
строка с $(echo "вложенной 'кавычкой'") и \"экранированием\"
"

raw='одинарные
кавычки'

ansi=$'табуляция\tи \' кавычка'
echo ${NAME%.*} ${NAME##*/} ${NAME//a/b} "${NAME:0:1}" ${#NAME} $# $$ $?
echo `date` "`hostname`" a#b \# c
echo 16#ff $((16#ff)) >&2 2>/dev/null
ls | grep -v x |
    sort &&
    echo конец ||
    echo ошибка

cat <<-TABS
	отступ табуляцией
	TABS

tee out.txt <<< "здесь-строка"
[[ -z "${x:-}" ]] && echo пусто
[[ $NAME =~ ^(a|b)$ ]] || true
echo a; echo b; echo c
sleep 1 &
wait
main() { echo тело; }
main
exit 0
