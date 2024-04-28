When a software project encounters a non-domain-specific problem, such as
solving a large linear system, controller synthesis, executing paint
operations, verifying a batch of cryptographic signatures, a common approach in
contemporary software engineering is reaching for a 'library' which already
'implements' some required algorithm for its solution. There are significant
problems with this approach:

- Not all languages scale with large project sizes. It's particularly tragic
  for compiled languages in comparison to scripting languages. Rarely is the
  trade-off of decreased developer productivity from escalating
  edit-compile-times versus some efficiency gains from avoiding
  serialization-like overheads from static data representations a choice.

- Many users find their business case does not care about the specifics of the
  solution, only the result. Designing with a specific library makes it
  fallaciously simple to couple too strongly to any specific approach though,
  for instance by putting off the task of plain descriptions of the actual
  problem in favor of ad-hoc solutions motivated only by the implementation
  language. And if one wants to have clear interfaces, micro-services being a
  pretty extreme approach, the implementation commonly finds itself with all
  the overheads of HTTPS and sockets. This library aims to be ergonomically
  competitive.

- It's hard to mix implementation languages. We're currently experiencing the
  nth cycle of rewriting UIs, yet the deeper numerics stay in Fortran. One
  shouldn't have to wait on any new language's Fortran/R/C/C++/Rust integration
  for algorithm implementation written in them to be well-usable. The
  server-client approach, modeled with micro-kernel IPC in mind, intends to
  replace some of the clumsiness of starting whole new processes that take
  underspecified, ad-hoc, custom-serialized stdin parameters to utilize such
  components. It allows such an approach to work over the shared memory
  exchange instead, also, but allows it to be more transparent as to whether a
  process must be started to serve each invocation.

- Many solver implementations do a host of other things besides solving the
  specific problem. However, the library approach keeps remnants of these
  operations in your process state well beyond the interval of problem solving.
  For instance, they might leak, require, retain process-wide resources such as
  file descriptors, memory, etc. For programs that require a high degree of
  assurance with regards to its process state, this is problematic even where
  results computed in independent, low-assurance processes would be otherwise
  fine. Personally, I'm convinced that large software stability issues can be
  wholly attributed to the complex, mangled process state cumulating naturally
  and unnoticed; libraries can't know the rest of their eventual process' state
  and thus can poorly design for finitely containing it.

- Expanding on state changed, security issues like that of XZ's infiltration
  must not be underestimated. Dynamic libraries are effective lent another
  program's credentials when they are utilized. This implicit privilege can be
  exploited better than containing it to a separate process as by high-jacking
  a function symbol. Writing security profiles (AppArmor, Landlock, SELinux,
  etc.) for well-contained processes is also simpler where they can only be
  applied process-wide and providing a process per problem naturally makes that
  set of necessary capabilities more well-defined and contains data on a more
  appropriate need-to-know basis whereas it would be implicitly shared within a
  process instead. (One might think of entirely outsourcing the handling of key
  material to a separate process doing no device IO of its own).

- Dynamic libraries only share code, not mutable data regions. If a
  pre-computable but large table is necessary for an algorithm, it will
  commonly not become shared between programs. Similarly, some caches might
  only ever hit when preserved between runs. This then often motivates a
  micro-service architecture critiqued above.

- An eventual goal is to make it feasible for _hardware devices_ to join the
  ring, hence also the near fit to existing hardware solutions. Computing a
  large tensor product should be possible for a process as well as hardware,
  with dispatch depending on the user and system environment. The point in
  externalizing the solver selection is in ensuring that verification of the
  client program for such an accelerator can occur much more independent of
  hardware availability, compared to the program having to interact with
  devices and drives from the kernel directly. It's maybe the biggest gamble
  whether this is really a competitive approach or whether device configuration
  requires the existing customization to such a degree as to be too hard to
  emulate for such verification.

- The memory mapping established for a ring _can_ be used as the backing memory
  of an `AF_XDP` socket. The library should let us do zero-copy routing of
  incoming frames towards other processes (and back). This should be a
  convincing template for similar hardware/software approaches, with focus on
  to their drivers interacting with existing memory pages allocated rather
  freely by user-space, but I also just like dabbling with network stacks. I'm
  also making the argument that, in the near future, an awareness and need for
  energy efficiency will lead to an increased interest in special-purpose
  accelerator hardware in general where NPU / TPU are just a start.
