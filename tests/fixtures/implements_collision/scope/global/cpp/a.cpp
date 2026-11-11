struct Global {};

namespace N {
// C++ looks for `Global` in `N`, then in the global namespace.
struct Child : Global {};
}
