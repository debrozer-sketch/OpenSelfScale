# shtrih-m

Драйвер весов Штрих-М (Слим) по последовательному порту.

Протокол снят живым дампом strace с работающих весов (172.32.0.5), все
константы и кадры в тестах — реальные байты с железа. Подробности: `audit/PROTOCOL.md`.

## Использование

```rust
use shtrih_m::ShtrihScales;
use std::time::Duration;

let port = serialport::new("/dev/scales", 115_200)
    .timeout(Duration::from_millis(900))
    .open()?;
let mut scales = ShtrihScales::new(port);
let reply = scales.poll_once(Duration::from_millis(900))?;
println!("{}.{:03} кг", reply.mass_g / 1000, reply.mass_g % 1000);
```

Порт 115200 8N1, цикл опроса непрерывный; LRC = XOR байтов после STX.
Ответ может приходить раздельными чтениями — драйвер собирает кадр сам.

## Тесты

`cargo test -p shtrih-m` — кадры сверяются с дампом (запрос, пустая платформа,
1350 г, битый LRC).
