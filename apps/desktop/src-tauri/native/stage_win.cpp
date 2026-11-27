// The picture is an owned popup above the WebView2 window. A child HWND of
// the main window is covered by the web view, so the screen would never
// appear. The popup follows the stage rectangle and does not take clicks.
#include <windows.h>
#include <GL/gl.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

#ifndef GL_R8
#define GL_R8 0x8229
#endif
#ifndef GL_RG
#define GL_RG 0x8227
#endif
#ifndef GL_RG8
#define GL_RG8 0x822B
#endif
#ifndef GL_TEXTURE0
#define GL_TEXTURE0 0x84C0
#endif
#ifndef GL_TEXTURE1
#define GL_TEXTURE1 0x84C1
#endif
#ifndef GL_CLAMP_TO_EDGE
#define GL_CLAMP_TO_EDGE 0x812F
#endif
#ifndef GL_ARRAY_BUFFER
#define GL_ARRAY_BUFFER 0x8892
#endif
#ifndef GL_STATIC_DRAW
#define GL_STATIC_DRAW 0x88E4
#endif
#ifndef GL_FRAGMENT_SHADER
#define GL_FRAGMENT_SHADER 0x8B30
#endif
#ifndef GL_VERTEX_SHADER
#define GL_VERTEX_SHADER 0x8B31
#endif
#ifndef GL_COMPILE_STATUS
#define GL_COMPILE_STATUS 0x8B81
#endif
#ifndef GL_LINK_STATUS
#define GL_LINK_STATUS 0x8B82
#endif
#ifndef GL_INFO_LOG_LENGTH
#define GL_INFO_LOG_LENGTH 0x8B84
#endif

typedef char GLchar;
typedef ptrdiff_t GLsizeiptr;
typedef GLuint(APIENTRY* PFN_glCreateShader)(GLenum);
typedef void(APIENTRY* PFN_glShaderSource)(GLuint, GLsizei, const GLchar**, const GLint*);
typedef void(APIENTRY* PFN_glCompileShader)(GLuint);
typedef void(APIENTRY* PFN_glGetShaderiv)(GLuint, GLenum, GLint*);
typedef void(APIENTRY* PFN_glGetShaderInfoLog)(GLuint, GLsizei, GLsizei*, GLchar*);
typedef void(APIENTRY* PFN_glDeleteShader)(GLuint);
typedef GLuint(APIENTRY* PFN_glCreateProgram)(void);
typedef void(APIENTRY* PFN_glAttachShader)(GLuint, GLuint);
typedef void(APIENTRY* PFN_glBindAttribLocation)(GLuint, GLuint, const GLchar*);
typedef void(APIENTRY* PFN_glLinkProgram)(GLuint);
typedef void(APIENTRY* PFN_glGetProgramiv)(GLuint, GLenum, GLint*);
typedef void(APIENTRY* PFN_glGetProgramInfoLog)(GLuint, GLsizei, GLsizei*, GLchar*);
typedef void(APIENTRY* PFN_glDeleteProgram)(GLuint);
typedef void(APIENTRY* PFN_glUseProgram)(GLuint);
typedef GLint(APIENTRY* PFN_glGetUniformLocation)(GLuint, const GLchar*);
typedef void(APIENTRY* PFN_glUniform1i)(GLint, GLint);
typedef void(APIENTRY* PFN_glUniform1f)(GLint, GLfloat);
typedef void(APIENTRY* PFN_glUniform2f)(GLint, GLfloat, GLfloat);
typedef void(APIENTRY* PFN_glGenVertexArrays)(GLsizei, GLuint*);
typedef void(APIENTRY* PFN_glBindVertexArray)(GLuint);
typedef void(APIENTRY* PFN_glGenBuffers)(GLsizei, GLuint*);
typedef void(APIENTRY* PFN_glBindBuffer)(GLenum, GLuint);
typedef void(APIENTRY* PFN_glBufferData)(GLenum, GLsizeiptr, const void*, GLenum);
typedef void(APIENTRY* PFN_glEnableVertexAttribArray)(GLuint);
typedef void(APIENTRY* PFN_glVertexAttribPointer)(GLuint, GLint, GLenum, GLboolean, GLsizei, const void*);
typedef void(APIENTRY* PFN_glActiveTexture)(GLenum);

struct Fns {
    PFN_glCreateShader CreateShader;
    PFN_glShaderSource ShaderSource;
    PFN_glCompileShader CompileShader;
    PFN_glGetShaderiv GetShaderiv;
    PFN_glGetShaderInfoLog GetShaderInfoLog;
    PFN_glDeleteShader DeleteShader;
    PFN_glCreateProgram CreateProgram;
    PFN_glAttachShader AttachShader;
    PFN_glBindAttribLocation BindAttribLocation;
    PFN_glLinkProgram LinkProgram;
    PFN_glGetProgramiv GetProgramiv;
    PFN_glGetProgramInfoLog GetProgramInfoLog;
    PFN_glDeleteProgram DeleteProgram;
    PFN_glUseProgram UseProgram;
    PFN_glGetUniformLocation GetUniformLocation;
    PFN_glUniform1i Uniform1i;
    PFN_glUniform1f Uniform1f;
    PFN_glUniform2f Uniform2f;
    PFN_glGenVertexArrays GenVertexArrays;
    PFN_glBindVertexArray BindVertexArray;
    PFN_glGenBuffers GenBuffers;
    PFN_glBindBuffer BindBuffer;
    PFN_glBufferData BufferData;
    PFN_glEnableVertexAttribArray EnableVertexAttribArray;
    PFN_glVertexAttribPointer VertexAttribPointer;
    PFN_glActiveTexture ActiveTexture;
    int loaded;
};

struct Stage {
    HWND hwnd;
    HDC dc;
    HGLRC rc;
    GLuint program;
    GLuint y_tex;
    GLuint c_tex;
    GLuint vao;
    GLint fit_loc;
    GLint range_loc;
    int tex_w;
    int tex_h;
    int win_w;
    int win_h;
    int failed;
};

static Fns g_fns;
static const wchar_t* kClass = L"MyCordStage";

static void log_line(const char* text) {
    OutputDebugStringA(text);
    OutputDebugStringA("\n");
}

static int load_fns(void) {
    if (g_fns.loaded) {
        return 1;
    }
#define LOAD(field, name) \
    g_fns.field = (decltype(g_fns.field))wglGetProcAddress(name); \
    if (!g_fns.field) { \
        log_line("picture surface: missing " name); \
        return 0; \
    }
    LOAD(CreateShader, "glCreateShader");
    LOAD(ShaderSource, "glShaderSource");
    LOAD(CompileShader, "glCompileShader");
    LOAD(GetShaderiv, "glGetShaderiv");
    LOAD(GetShaderInfoLog, "glGetShaderInfoLog");
    LOAD(DeleteShader, "glDeleteShader");
    LOAD(CreateProgram, "glCreateProgram");
    LOAD(AttachShader, "glAttachShader");
    LOAD(BindAttribLocation, "glBindAttribLocation");
    LOAD(LinkProgram, "glLinkProgram");
    LOAD(GetProgramiv, "glGetProgramiv");
    LOAD(GetProgramInfoLog, "glGetProgramInfoLog");
    LOAD(DeleteProgram, "glDeleteProgram");
    LOAD(UseProgram, "glUseProgram");
    LOAD(GetUniformLocation, "glGetUniformLocation");
    LOAD(Uniform1i, "glUniform1i");
    LOAD(Uniform1f, "glUniform1f");
    LOAD(Uniform2f, "glUniform2f");
    LOAD(GenVertexArrays, "glGenVertexArrays");
    LOAD(BindVertexArray, "glBindVertexArray");
    LOAD(GenBuffers, "glGenBuffers");
    LOAD(BindBuffer, "glBindBuffer");
    LOAD(BufferData, "glBufferData");
    LOAD(EnableVertexAttribArray, "glEnableVertexAttribArray");
    LOAD(VertexAttribPointer, "glVertexAttribPointer");
    LOAD(ActiveTexture, "glActiveTexture");
#undef LOAD
    g_fns.loaded = 1;
    return 1;
}

static const char* kVertex =
    "#version 150\n"
    "in vec2 pos;\n"
    "uniform vec2 fit;\n"
    "out vec2 uv;\n"
    "void main() {\n"
    "  uv = vec2(pos.x * 0.5 + 0.5, 0.5 - pos.y * 0.5);\n"
    "  gl_Position = vec4(pos * fit, 0.0, 1.0);\n"
    "}\n";

static const char* kFragment =
    "#version 150\n"
    "uniform sampler2D y_tex;\n"
    "uniform sampler2D c_tex;\n"
    "uniform float full_range;\n"
    "in vec2 uv;\n"
    "out vec4 color;\n"
    "void main() {\n"
    "  float y = texture(y_tex, uv).r;\n"
    "  vec2 c = texture(c_tex, uv).rg - vec2(0.5);\n"
    "  if (full_range < 0.5) y = (y - 0.062745) * 1.164384;\n"
    "  float r = y + 1.792741 * c.y;\n"
    "  float g = y - 0.213249 * c.x - 0.532909 * c.y;\n"
    "  float b = y + 2.112402 * c.x;\n"
    "  color = vec4(clamp(vec3(r, g, b), 0.0, 1.0), 1.0);\n"
    "}\n";

static GLuint compile_shader(GLenum kind, const char* source) {
    GLuint shader = g_fns.CreateShader(kind);
    g_fns.ShaderSource(shader, 1, &source, nullptr);
    g_fns.CompileShader(shader);
    GLint ok = 0;
    g_fns.GetShaderiv(shader, GL_COMPILE_STATUS, &ok);
    if (!ok) {
        char info[512];
        info[0] = 0;
        g_fns.GetShaderInfoLog(shader, (GLsizei)sizeof(info), nullptr, info);
        log_line(info);
        g_fns.DeleteShader(shader);
        return 0;
    }
    return shader;
}

static int build_program(Stage* stage) {
    if (!load_fns()) {
        return 0;
    }
    GLuint vertex = compile_shader(GL_VERTEX_SHADER, kVertex);
    GLuint fragment = compile_shader(GL_FRAGMENT_SHADER, kFragment);
    if (!vertex || !fragment) {
        return 0;
    }
    GLuint program = g_fns.CreateProgram();
    g_fns.AttachShader(program, vertex);
    g_fns.AttachShader(program, fragment);
    g_fns.BindAttribLocation(program, 0, "pos");
    g_fns.LinkProgram(program);
    g_fns.DeleteShader(vertex);
    g_fns.DeleteShader(fragment);
    GLint ok = 0;
    g_fns.GetProgramiv(program, GL_LINK_STATUS, &ok);
    if (!ok) {
        char info[512];
        info[0] = 0;
        g_fns.GetProgramInfoLog(program, (GLsizei)sizeof(info), nullptr, info);
        log_line(info);
        g_fns.DeleteProgram(program);
        return 0;
    }
    g_fns.UseProgram(program);
    GLint y_loc = g_fns.GetUniformLocation(program, "y_tex");
    GLint c_loc = g_fns.GetUniformLocation(program, "c_tex");
    if (y_loc >= 0) {
        g_fns.Uniform1i(y_loc, 0);
    }
    if (c_loc >= 0) {
        g_fns.Uniform1i(c_loc, 1);
    }
    stage->fit_loc = g_fns.GetUniformLocation(program, "fit");
    stage->range_loc = g_fns.GetUniformLocation(program, "full_range");

    float quad[8] = {-1.f, -1.f, 1.f, -1.f, -1.f, 1.f, 1.f, 1.f};
    g_fns.GenVertexArrays(1, &stage->vao);
    GLuint vbo = 0;
    g_fns.GenBuffers(1, &vbo);
    g_fns.BindVertexArray(stage->vao);
    g_fns.BindBuffer(GL_ARRAY_BUFFER, vbo);
    g_fns.BufferData(GL_ARRAY_BUFFER, sizeof(quad), quad, GL_STATIC_DRAW);
    g_fns.EnableVertexAttribArray(0);
    g_fns.VertexAttribPointer(0, 2, GL_FLOAT, GL_FALSE, 8, nullptr);
    g_fns.BindVertexArray(0);

    GLuint pair[2] = {0, 0};
    glGenTextures(2, pair);
    stage->y_tex = pair[0];
    stage->c_tex = pair[1];
    for (int i = 0; i < 2; i++) {
        glBindTexture(GL_TEXTURE_2D, pair[i]);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
    }
    stage->program = program;
    return 1;
}

static LRESULT CALLBACK stage_wndproc(HWND hwnd, UINT msg, WPARAM wparam, LPARAM lparam) {
    if (msg == WM_NCHITTEST) {
        return HTTRANSPARENT;
    }
    if (msg == WM_MOUSEACTIVATE) {
        return MA_NOACTIVATE;
    }
    if (msg == WM_ERASEBKGND) {
        return 1;
    }
    if (msg == WM_DESTROY) {
        Stage* stage = (Stage*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);
        if (stage) {
            if (stage->rc) {
                wglMakeCurrent(nullptr, nullptr);
                wglDeleteContext(stage->rc);
            }
            if (stage->dc) {
                ReleaseDC(hwnd, stage->dc);
            }
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            delete stage;
        }
    }
    return DefWindowProcW(hwnd, msg, wparam, lparam);
}

static void register_class(void) {
    static int registered = 0;
    if (registered) {
        return;
    }
    WNDCLASSW wc = {};
    wc.style = CS_OWNDC | CS_HREDRAW | CS_VREDRAW;
    wc.lpfnWndProc = stage_wndproc;
    wc.hInstance = GetModuleHandleW(nullptr);
    wc.lpszClassName = kClass;
    wc.hCursor = LoadCursorW(nullptr, (LPCWSTR)IDC_ARROW);
    registered = RegisterClassW(&wc) != 0 || GetLastError() == ERROR_CLASS_ALREADY_EXISTS;
}

static int init_gl(Stage* stage) {
    if (stage->failed) {
        return 0;
    }
    if (stage->program) {
        return 1;
    }
    PIXELFORMATDESCRIPTOR pfd = {};
    pfd.nSize = sizeof(pfd);
    pfd.nVersion = 1;
    pfd.dwFlags = PFD_DRAW_TO_WINDOW | PFD_SUPPORT_OPENGL | PFD_DOUBLEBUFFER;
    pfd.iPixelType = PFD_TYPE_RGBA;
    pfd.cColorBits = 32;
    int format = ChoosePixelFormat(stage->dc, &pfd);
    if (!format || !SetPixelFormat(stage->dc, format, &pfd)) {
        stage->failed = 1;
        log_line("picture surface: no pixel format");
        return 0;
    }
    HGLRC legacy = wglCreateContext(stage->dc);
    if (!legacy || !wglMakeCurrent(stage->dc, legacy)) {
        stage->failed = 1;
        log_line("picture surface: no GL context");
        return 0;
    }
    typedef HGLRC(WINAPI * CreateAttribs)(HDC, HGLRC, const int*);
    CreateAttribs create_attribs = (CreateAttribs)wglGetProcAddress("wglCreateContextAttribsARB");
    const int kMajor = 0x2091;
    const int kMinor = 0x2092;
    const int kProfile = 0x9126;
    const int kCompat = 0x00000002;
    int attribs[] = {kMajor, 3, kMinor, 2, kProfile, kCompat, 0};
    HGLRC modern = create_attribs ? create_attribs(stage->dc, nullptr, attribs) : nullptr;
    if (modern && wglMakeCurrent(stage->dc, modern)) {
        wglDeleteContext(legacy);
        stage->rc = modern;
    } else {
        wglMakeCurrent(stage->dc, legacy);
        stage->rc = legacy;
    }
    if (!build_program(stage)) {
        stage->failed = 1;
        log_line("picture surface: shader failed");
        return 0;
    }
    return 1;
}

extern "C" void* dcmax_stage_place(void* owner_ptr, void* existing, double x, double y, double w, double h, int visible) {
    HWND owner = (HWND)owner_ptr;
    HWND hwnd = (HWND)existing;
    if (!owner) {
        return existing;
    }
    if (!visible && !hwnd) {
        return existing;
    }
    register_class();
    if (!hwnd || !IsWindow(hwnd)) {
        hwnd = CreateWindowExW(
            WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TRANSPARENT,
            kClass,
            L"",
            WS_POPUP,
            0,
            0,
            2,
            2,
            owner,
            nullptr,
            GetModuleHandleW(nullptr),
            nullptr);
        if (!hwnd) {
            log_line("picture surface: window was not created");
            return nullptr;
        }
        Stage* stage = new Stage();
        memset(stage, 0, sizeof(Stage));
        stage->hwnd = hwnd;
        stage->dc = GetDC(hwnd);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, (LONG_PTR)stage);
    }
    POINT origin = {0, 0};
    ClientToScreen(owner, &origin);
    int width = (int)(w < 2.0 ? 2.0 : w);
    int height = (int)(h < 2.0 ? 2.0 : h);
    UINT flags = SWP_NOACTIVATE | SWP_NOZORDER;
    flags |= visible ? SWP_SHOWWINDOW : SWP_HIDEWINDOW;
    SetWindowPos(hwnd, nullptr, origin.x + (int)x, origin.y + (int)y, width, height, flags);
    return hwnd;
}

extern "C" void dcmax_stage_holes(void* window, const double* xywh, int count, double radius) {
    HWND hwnd = (HWND)window;
    if (!hwnd || !IsWindow(hwnd)) {
        return;
    }
    RECT client = {0, 0, 0, 0};
    GetClientRect(hwnd, &client);
    int width = client.right - client.left;
    int height = client.bottom - client.top;
    if (width < 2 || height < 2) {
        return;
    }
    int curve = (int)(radius * 2.0);
    if (curve < 0) {
        curve = 0;
    }
    HRGN region = curve > 0 ? CreateRoundRectRgn(0, 0, width + 1, height + 1, curve, curve) : CreateRectRgn(0, 0, width, height);
    for (int index = 0; index < count; index++) {
        int x = (int)xywh[index * 4];
        int y = (int)xywh[index * 4 + 1];
        int hole_w = (int)xywh[index * 4 + 2];
        int hole_h = (int)xywh[index * 4 + 3];
        if (hole_w < 1 || hole_h < 1) {
            continue;
        }
        HRGN hole = CreateRoundRectRgn(x, y, x + hole_w, y + hole_h, 24, 24);
        CombineRgn(region, region, hole, RGN_DIFF);
        DeleteObject(hole);
    }
    SetWindowRgn(hwnd, region, TRUE);
}

extern "C" int dcmax_stage_ready(void* window) {
    HWND hwnd = (HWND)window;
    if (!hwnd || !IsWindow(hwnd)) {
        return 0;
    }
    Stage* stage = (Stage*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if (!stage || !stage->dc) {
        return 0;
    }
    return init_gl(stage);
}

extern "C" int dcmax_stage_draw_nv12(void* window, const uint8_t* y, const uint8_t* uv, int width, int height, int full_range) {
    HWND hwnd = (HWND)window;
    if (!hwnd || !y || !uv || width < 2 || height < 2 || (width & 1) || (height & 1)) {
        return -1;
    }
    Stage* stage = (Stage*)GetWindowLongPtrW(hwnd, GWLP_USERDATA);
    if (!stage || !init_gl(stage)) {
        return -2;
    }
    if (!wglMakeCurrent(stage->dc, stage->rc)) {
        return -2;
    }
    RECT client;
    GetClientRect(hwnd, &client);
    int view_w = client.right - client.left;
    int view_h = client.bottom - client.top;
    if (view_w < 2 || view_h < 2) {
        return -2;
    }
    size_t y_bytes = (size_t)width * (size_t)height;
    glPixelStorei(GL_UNPACK_ALIGNMENT, 1);
    if (stage->tex_w != width || stage->tex_h != height) {
        glBindTexture(GL_TEXTURE_2D, stage->y_tex);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_R8, width, height, 0, GL_RED, GL_UNSIGNED_BYTE, y);
        glBindTexture(GL_TEXTURE_2D, stage->c_tex);
        glTexImage2D(GL_TEXTURE_2D, 0, GL_RG8, width / 2, height / 2, 0, GL_RG, GL_UNSIGNED_BYTE, uv);
        stage->tex_w = width;
        stage->tex_h = height;
    } else {
        glBindTexture(GL_TEXTURE_2D, stage->y_tex);
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, width, height, GL_RED, GL_UNSIGNED_BYTE, y);
        glBindTexture(GL_TEXTURE_2D, stage->c_tex);
        glTexSubImage2D(GL_TEXTURE_2D, 0, 0, 0, width / 2, height / 2, GL_RG, GL_UNSIGNED_BYTE, uv);
    }
    float content = (float)width / (float)height;
    float view = (float)view_w / (float)view_h;
    float fit_x = 1.f;
    float fit_y = 1.f;
    if (view > content) {
        fit_y = view / content;
    } else if (content > 0.f) {
        fit_x = content / view;
    }
    glViewport(0, 0, view_w, view_h);
    glDisable(GL_BLEND);
    glClearColor(0.f, 0.f, 0.f, 1.f);
    glClear(GL_COLOR_BUFFER_BIT);
    g_fns.UseProgram(stage->program);
    if (stage->fit_loc >= 0) {
        g_fns.Uniform2f(stage->fit_loc, fit_x, fit_y);
    }
    if (stage->range_loc >= 0) {
        g_fns.Uniform1f(stage->range_loc, full_range ? 1.f : 0.f);
    }
    g_fns.ActiveTexture(GL_TEXTURE0);
    glBindTexture(GL_TEXTURE_2D, stage->y_tex);
    g_fns.ActiveTexture(GL_TEXTURE1);
    glBindTexture(GL_TEXTURE_2D, stage->c_tex);
    g_fns.BindVertexArray(stage->vao);
    glDrawArrays(GL_TRIANGLE_STRIP, 0, 4);
    g_fns.BindVertexArray(0);
    SwapBuffers(stage->dc);
    (void)y_bytes;
    return 0;
}

// Win+. is the system emoji panel. It types into whichever field is focused.
extern "C" void dcmax_emoji_panel(void) {
    INPUT chord[4] = {};
    chord[0].type = INPUT_KEYBOARD;
    chord[0].ki.wVk = VK_LWIN;
    chord[1].type = INPUT_KEYBOARD;
    chord[1].ki.wVk = VK_OEM_PERIOD;
    chord[2].type = INPUT_KEYBOARD;
    chord[2].ki.wVk = VK_OEM_PERIOD;
    chord[2].ki.dwFlags = KEYEVENTF_KEYUP;
    chord[3].type = INPUT_KEYBOARD;
    chord[3].ki.wVk = VK_LWIN;
    chord[3].ki.dwFlags = KEYEVENTF_KEYUP;
    SendInput(4, chord, sizeof(INPUT));
}
