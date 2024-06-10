use std::io::{BufRead as _, Write as _};

#[no_mangle]
pub fn main() -> Result<(), std::io::Error> {
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();

    let stdin = stdin.lock();
    let mut stdout = stdout.lock();

    for line in stdin.lines() {
        let Ok(line) = line else {
            writeln!(&mut stdout, "Error:IO")?;
            continue;
        };

        let Ok(num) = line.parse::<u64>() else {
            writeln!(&mut stdout, "Error:NUM")?;
            continue;
        };

        if num % 15 == 0 {
            writeln!(&mut stdout, "FizzBuzz")?;
        } else if num % 5 == 0 {
            writeln!(&mut stdout, "Buzz")?;
        } else if num % 3 == 0 {
            writeln!(&mut stdout, "Fizz")?;
        } else {
            writeln!(&mut stdout, "{}", num)?;
        }
    }

    Ok(())
}
