# 01 · 双层适配器模式（所有模块共用规范）

> 本规范是拆分能在"只拆不写"约束下成立的核心机制。任何模块的
> C API 设计与适配类实现都必须照此执行。命名、内存所有权、错误码、
> 线程与信号的处理在此**冻结**。

## 0. 接口铁律（2026-08 修订，优先级高于本文件其余各节）

模块边界上的接口必须是**纯 C 的、面向对象的**：

1. **对象 = 不透明句柄**。每个跨界类型是一个不透明结构体指针：
   `typedef struct OakModClazz OakModClazz;`（**手写 struct 标签的不透明
   指针**，禁止用 `void *` 充当对象——现有代码里 `void *parent_qobject`、
   `void *oaktask_import_take_command()` 这类用法是反面教材，新接口一律
   禁止，旧接口在所属模块拆分时顺手改为有类型句柄）。
2. **构造/析构 = init/free 函数对**：`oakmod_clazz_init*()` /
   `oakmod_clazz_free()`。free 对 NULL 是 no-op。
3. **成员函数 = 首参为 self 句柄的普通函数**：
   `oakmod_clazz_<func>(OakModClazz *self, ...)`；静态成员函数无 self
   （`_s` 后缀）。
4. **多态 = 手工虚函数表**。确需"基类句柄 + 多种实现"（如存储后端、
   undo 命令、渲染后端插件）时，在公共头里定义纯 C 函数指针表
   （`typedef struct { ...; int (*save)(...); ... } OakModClazzVTable;`），
   提供侧填充、消费侧经表调用，**不得让 C++ vtable 跨界**。模板见
   `M10-oakstorage.md` §2.3 的 `OakStorageBackend`。
5. **禁止 C++ 对象跨越动态库边界**：边界上只出现 C 类型（整数、double、
   指针、`const char *`、纯 C POD、不透明句柄）。C++ 类实例、引用、
   `std::` 类型、Qt 类型一律不得出现在任何模块的 `include/` 公共头里。
6. **禁止跨界调用 C++ 成员函数**：消费侧对提供侧对象的一切操作必须经
   该对象的 C ABI 函数；拿到句柄后 `reinterpret_cast` 回 C++ 指针再调
   成员函数视为违规（nm 审计 + 代码评审双保险）。

## 1. 提供侧：C API 层

对被消费的每个 C++ 类 `Clazz`，模块在 `include/oak<mod>/clazz.h`
里暴露一组 `extern "C"` 函数：

```c
/* 构造/析构 */
OAKMOD_API OakModClazz *oakmod_clazz_init(/* 与某个构造函数对应的参数 */);
OAKMOD_API void       oakmod_clazz_free(OakModClazz *self);

/* 普通成员函数：self 为第一参数，其余参数按 §3 POD 化 */
OAKMOD_API <ret>    oakmod_clazz_<func>(OakModClazz *self, ...);

/* 静态成员函数：无 self */
OAKMOD_API <ret>    oakmod_clazz_<func>_s(/* 参数 */);
```

规则：

1. `OakModClazz` 是**不透明类型**（`typedef struct OakModClazz OakModClazz;`），
   提供侧实现里它就是 `olive::Clazz*`（`reinterpret_cast`），消费侧
   永远无法解引用。
2. `init` 每个对应一个实际在用的构造函数重载；返回 NULL 表示失败。
   `free` 对 NULL 是 no-op。**所有权：init 创建的对象归调用方，必须
   配对 free**；借用指针（不转移所有权）在文档注释里写 `/* borrowed */`。
3. 多个构造重载用后缀区分：`oakmod_clazz_init`（默认）、
   `oakmod_clazz_init_from_file`、`oakmod_clazz_init_copy` 等。
4. 命名全小写，模块前缀 `oak<mod>_`（oakundo/oaknode/oaktimeline/
   oakcodec/oakrender/oaktask/oakaudio/oakplugin/oak_core/oakstorage）。
5. 导出宏 `OAKMOD_API` 照 `oakengine/export.h` 样式
   （`__attribute__((visibility("default")))`），模块编译加
   `-fvisibility=hidden`——每个模块**出生即 visibility 干净**，
   R7-B 的全局收口变成顺水推舟。

## 2. 消费侧：同名 C++ 适配类

消费模块里放一个与原始类**同名**的适配类（放在消费侧私有头
`<mod>/adapter/clazz.h`，namespace 保持 `olive`）：

```cpp
namespace olive {

class Clazz {
public:
	Clazz(/* 原构造签名 */)
		: h_(oakmod_clazz_init(/* 相同参数 */)) {}
	~Clazz() { oakmod_clazz_free(h_); }

	// 原成员函数签名原样保留，体内转发一行
	Ret func(Arg a) { return oakmod_clazz_func(h_, a); }

	// 句柄逃生口（确实需要混用时的过渡手段，注释标注）
	OakModClazz *handle() const { return h_; }

private:
	OakModClazz *h_;
	// 拷贝语义与原类一致：原类不可拷贝就 = delete；
	// 原类可拷贝则 init_copy。
};

}
```

效果：消费模块里所有 `clazz.func(a)`、`new Clazz(...)`、
`clazz->func(a)` 调用点**零改动**——原来 include 提供侧 C++ 头的
地方，改 include 本模块的适配头即可。这就是"双层适配器"：
**C++ 调用点 → 同名适配类 → C ABI → 提供侧 C++ 实现**。

注意：

- 适配类是**逐模块私有的**：oakrender 消费的 Node 适配类和
  oaktimeline 消费的 Node 适配类是两份独立的小头文件，各自只有
  自己用到的方法子集。允许重复，禁止共享（共享就重新耦合了）。
- 适配类只转发**本模块实际用到**的方法（按 02 的依赖矩阵逐个
  grep 确认清单，写在模块手册里）。
- 原类有继承体系的（如 UndoCommand 子类族），见 §5。

## 3. 参数与返回值的 POD 化

C ABI 上只允许：整数、`double`、`int64_t`、指针、`const char *`、
以及 `include/oak<mod>/types.h` 里定义的纯 C POD（无构造/析构/
方法）。Qt/C++ 类型按下表映射：

| C++ 类型 | C ABI 形态 |
|---|---|
| `QString` | `const char *`（入，UTF-8）；出：buf/size 两段式（先 NULL 查长度） |
| `olive::core::Rational` | 两个 `int64_t`（num, den）；时间戳用 `int64_t` 帧戳 |
| `TimeRange` | 两个 `int64_t`（in_ts, out_ts） |
| `QVariant`/`NodeValue` | `oak_node_value` POD（oakengine/node.h 已有，各模块复用该定义或复制同构 POD） |
| `VideoParams`/`AudioParams` | `oak_video_params`/POD 字段拍平 |
| `QList<T>`/`QVector<T>` | 指针 + count；返回集合用 `count` + `at(i)` 访问器对 |
| `Color` | 4 × double |
| `Qt::enum`/内部枚举 | `int`（取值表写进手册，两侧枚举**序数一致性**用 static_assert 或测试钉死） |
| `std::shared_ptr<T>` | 不透明句柄 + retain/free（协议见 display.h R7-A 的先例） |

## 4. 信号、回调与线程（2026-08 修订：上层对下层只有命令）

Qt 信号不许跨模块；**下层对上层也不许持有回调**——上层调用下层时
必须知道其影响（改了什么全在返回值/出参里），变更通知由**调用方所在
层**发出，不经下层反向通知。各模块 C ABI 因此一律不含
subscribe/unsubscribe 类函数。处理规则：

1. **同步命令**：提供侧把结果放在返回值/出参；消费侧适配类在调用后
   自行发 Qt 信号（适配类知道刚执行了什么命令，见 §7 例）。原
   `connect()` 到适配类信号的 widget 代码零改动。
2. **唯一例外——异步任务**：后台执行单元（oaktask 任务、oakrender
   渲染 ticket）提交时拿不到结果，允许进度/完成回调作为该命令的
   返回通道：`oak<mod>_<async>_start(handle, done_fn, userdata)` 形式，
   一次性语义，完成后自动失效。
3. 线程语义照现状：异步回调在发射线程同步调用（DirectConnection
   等价），需要跨线程排队是消费侧适配类自己的事（它可以用
   `QMetaObject::invokeMethod(..., Qt::QueuedConnection)`）。
4. **facade→app 的 `oakengine_event` 通道不在此列**：那是引擎对最外层
   的唯一通知机制（riir.md §6.1），事件由 facade 在命令完成后发射，
   不由下层模块直接发射。

## 5. 继承与虚函数

原类的继承体系**不出模块**。跨界时：

- 基类引用（如 `UndoCommand *`）→ 基类句柄（`OakUndoCommand *`）。
- 消费侧不构造具体子类，只经提供侧的工厂函数：
  `oakmod_clazz_create_<variant>(...)`（返回基类句柄）。
- 消费侧确实需要子类行为的（极少数，需逐个人工论证），在提供侧
  加专用函数，不放虚函数跨界。

## 6. 错误码

沿用 facade 约定：`OAKMOD_OK=0`，负值错误
（`OAKMOD_E_INVALID/E_STATE/E_NOT_FOUND/E_FAILED`），在各模块
`types.h` 里统一定义（值与 oakengine 的现有值对齐）。会失败的
`init` 返回 NULL，错误细节经 `oakmod_last_error(buf, size)` 取
（线程局部，照 facade 的 `set_error` 模式）。

## 7. 一个完整例子（UndoStack，M2 照此落地）

```c
/* oakundo/include/oakundo/undostack.h */
typedef struct OakUndoStack OakUndoStack;
typedef struct OakUndoObjectParent OakUndoObjectParent;  /* borrowed QObject 挂载点 */
OAKUNDO_API OakUndoStack *oakundo_undostack_init(
	const OakUndoObjectParent *parent);
OAKUNDO_API void oakundo_undostack_free(OakUndoStack *self);
OAKUNDO_API void oakundo_undostack_push(OakUndoStack *self,
	OakUndoCommand *cmd, const char *name);
OAKUNDO_API int  oakundo_undostack_can_undo(const OakUndoStack *self);
OAKUNDO_API int64_t oakundo_undostack_index(const OakUndoStack *self);
/* ... 完整表见 M2 手册（纯命令接口，无任何 subscribe） ... */
```

```cpp
// 消费侧 adapter/undostack.h —— app/other 模块里 connect() 零改动。
// 通知规则（§4）：适配类发了变更命令，它知道影响，信号由适配类自己 emit。
class UndoStack : public QObject {
	Q_OBJECT
public:
	explicit UndoStack(QObject *p = nullptr)
		: QObject(p), h_(oakundo_undostack_init(
			reinterpret_cast<const OakUndoObjectParent *>(p))) {}
	~UndoStack() override { oakundo_undostack_free(h_); }
	void push(UndoCommand *c, const QString &n) {
		oakundo_undostack_push(h_, c->handle(), n.toUtf8().constData());
		emit index_changed(int(oakundo_undostack_index(h_)));  // 命令后自发通知
	}
	bool canUndo() const { return oakundo_undostack_can_undo(h_) != 0; }
signals:
	void index_changed(int);
private:
	OakUndoStack *h_;
};
```
